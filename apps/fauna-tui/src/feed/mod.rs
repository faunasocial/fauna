//! The feed page — tui's first authenticated page, and the template every later
//! one follows.
//!
//! **Where the logic lives** (`feed.md` § Where logic lives): the snapshot and
//! every mutation belong to the shared [`fauna_feed::FeedManager`], which tui
//! consumes **directly** — no FFI hop, the same way linux does (priority #2:
//! the two Rust-native apps use the generic manager). This module is a paint
//! shell: it reads a snapshot, emits an [`Element`] list, and forwards gestures
//! to manager methods. It holds no view-model state that the manager could own.
//!
//! The three things that *are* client glue, and why:
//!
//! 1. **The blob upload** — it rides the platform HTTP/bulk plane, not WS-RPC.
//!    Shared with linux all the same ([`fauna_client::upload_public_post_blob`]);
//!    only the file *staging* is local.
//! 2. **The interaction bar** — `feed.md` § User actions puts interact on
//!    `PostsClient`, not on the manager.
//! 3. **The resolve triggers** — lazily folding embeds into the document is a
//!    view concern; the manager exposes the idempotent methods and the page
//!    decides when to fire them.
//!
//! **The element rule this page inherits from the walker** (`document.rs`): a
//! block that ui.yaml gives its own element ID is painted by the **page**, as
//! that element; everything else is body text painted by the walker. So
//! `post-image`, `quoted-post` and `link-preview-card` are extracted here via
//! the shared projections and registered as their own elements, and the walker
//! treats those blocks as inert. Painting in both places double-renders.

pub mod cues;
pub mod drafts;

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client::MultipartBlob;
use fauna_client::NestClient;
use fauna_core::obligation::RenderVerdict;
use fauna_core::render::{AuthoringOriginStatus, VerificationStatus};
use fauna_feed::{
    AttachedFile, FactorWeightInput, FeedManager, FeedSnapshot, FeedSnapshotObserver,
    FilterRuleInput, PostSummary, ReplyAudience, SourceKind, classify_sources,
};
// Entering sell mode is the paywall-designation gesture, the money plane's
// (`dynamic-features.md` § Platform-family surface excision → *The
// price-and-route class*); only the `payments` flavor stages a sale.
#[cfg(feature = "payments")]
use fauna_feed::SellComposeState;
use fauna_i18n::strings::{common, composer, conversations, family, feed};
// The payment-link strings are read only by the money plane's two gated
// arms (`OpenPaymentLink`, the sold-post teaser) — gated with them.
#[cfg(feature = "payments")]
use fauna_i18n::strings::subscriptions;
// The post tip surface's strings (`monetization.md` § Tips). Aliased because
// `tips` would collide with the `PostSummary.tips` field name at every use site
// in this module, which is exactly where the strings are read.
#[cfg(feature = "payments")]
use fauna_i18n::strings::tips as tips_i18n;
use fauna_nest_http::{NestContentApi, paths};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{App, DataMessage, UiMessage};
use crate::element::{Element, Field, Gesture, SelectTarget};
use crate::image_cache::{ImageCache, ImageState};
use fauna_core::load_cache::Finished;

/// tui's feed manager: the generic manager over the authed WS-RPC transport,
/// consumed with no FFI hop (the 2nd direct-Rust consumer, after linux).
pub type CliFeedManager = FeedManager<Arc<NestClient>>;

/// The canonical `feed-rule-type-select` catalog — the shared
/// `fauna_client_feed::rule_type_options()`: wire value (what the select
/// selects by and what `FeedManager::create_feed` encodes), localized label
/// (the paint-only prompt), and the [`fauna_client_feed::RuleInputKind`] that
/// decides which input widgets the rule row shows (the same source linux
/// binds — `feed.md` § Where logic lives → Feed rule-builder presentation).
fn rule_type_options() -> Vec<fauna_client_feed::RuleTypeOption> {
    fauna_client_feed::rule_type_options()
}

/// A gesture on the feed page. Each maps onto a manager method, a `PostsClient`
/// call, or a local form edit — never a navigation decision of its own.
#[derive(Debug, Clone)]
pub enum Action {
    SelectFeed(String),
    /// `feed-trending-item` — select the built-in **Trending** virtual feed
    /// (`trending.md` § The Trending feed). Carries no id: the virtual feed has
    /// no feed row, so the shared manager keys it on `trending_selected` rather
    /// than a `selected_feed` value, mirroring `SelectFeed`'s `None` = local.
    SelectTrending,
    /// `feed-post-actions-button` — open (or re-open) a post card's ⋯ overflow.
    OpenPostActions(String),
    /// `feed-post-{more,less}-like-this` — train the open post into the
    /// in-context trained factor, or un-mark it when its own verb is already
    /// active (the toggle semantics ui.yaml's component spec states).
    TrainPost {
        post_id: String,
        verb: fauna_feed::TrainVerb,
    },
    /// `feed-post-delete-button` — arm the destructive confirm step on this own
    /// post. Purely local: nothing is destroyed until the confirm.
    StartDeletePost(String),
    /// `feed-post-delete-confirm-button` — destroy this own post through the
    /// shared `PostsClient::posts_delete` (`feed.md` § State & data shape →
    /// *Post deletion*). The author-only destruction verb, and the one
    /// sanctioned "data loss" — the *user always controls their data*
    /// invariant's delete affordance.
    ConfirmDeletePost(String),
    /// `feed-post-publish-web-button` — publish this own post as a web page at
    /// the nest's default slug (`fauna.web.publish.set`).
    PublishWeb(String),
    /// `feed-post-unpublish-web-button` — take the published page down
    /// (`fauna.web.publish.unset`). One tap, no confirm step: unlike delete it
    /// is idempotent and reversible (`web-content-hosting.md` § Published-post
    /// management).
    UnpublishWeb(String),
    /// `feed-post-copy-web-link-button` — copy the tokenless public page URL
    /// (the selling link; a visitor with no token gets the teaser).
    CopyWebLink(String),
    /// `feed-post-copy-paywall-link-button` — mint and copy a short-lived
    /// full-access URL for a published **and** gated post.
    CopyPaywallLink(String),
    ClearSearch,
    SubmitPost,
    /// `compose-dialog-button` — open the rich compose dialog
    /// (`feed-compose-dialog`). Dismissed with Escape: ui.yaml declares no
    /// dismiss element inside the dialog, the same keymap-only shape the feed
    /// sub-pages and the conversations overlays use.
    OpenComposeDialog,
    /// `post-image` click — open the full-screen `image-lightbox` over that
    /// post's image, by blob hash. The hash (not a row index) because the
    /// loaded window re-ranks in place, the `OpenPostActions` finding.
    OpenLightbox(String),
    /// `post-tip-list-button` — open that post's `post-tip-list` attribution
    /// window (`monetization.md` § Tips). By post id, same re-rank reasoning as
    /// [`Self::OpenLightbox`]. Purely local: the window is already resolved
    /// onto `PostSummary.tips` by the time the button renders, so opening it
    /// costs no round trip.
    #[cfg(feature = "payments")]
    OpenTipList(String),
    /// `compose-gate-tier-select` — the one control answering "who can read
    /// this?". Carries the chosen **display label**: the localized "Public",
    /// one of `own_tiers`' names, or the localized "Sell this post…" (linux's
    /// label round-trip, `post_list.rs:619`). The three answers are mutually
    /// exclusive, which the shared setters enforce.
    SetGateTier(String),
    /// `compose-sell-subscribers-free` — the ratified rank knob
    /// (`monetization.md:126`). Only reachable in sell mode — the money
    /// plane's, with the sell composer that paints it.
    #[cfg(feature = "payments")]
    ToggleSellSubscribersFree,
    /// `compose-file-remove` — drop the composer's attached file: the local
    /// path and the manager's handle together, whether the handle came from a
    /// pick on this device or from a restored draft whose bytes are not here.
    /// Staging `None` is the user's remove gesture, the one the ruling
    /// sanctions (`feed.md` § Persistence → *Attachments by content address*)
    /// — android's `FeedVM.clearComposeAttachment`, and
    /// `dm-compose-attachment-remove` one page over.
    RemoveComposeAttachment,
    /// `feed-{like,repost,quote}-button` — client glue over `PostsClient`.
    /// **Never `"reply"`** — reply needs a typed body `interact` cannot carry
    /// ([`Self::OpenReplyDialog`] is the button's real gesture now).
    Interact {
        post_id: String,
        action: String,
    },
    /// `feed-reply-button` — arm the reply-compose dialog (`feed-reply-dialog`)
    /// for this post. Purely local: nothing is sent until the dialog's own
    /// submit. Unlike `like`/`repost`/`quote`, reply cannot be a direct tap —
    /// `FeedManager::reply` refuses an empty body, and routing it through the
    /// raw `interact` door is exactly the bug this dialog exists to fix
    /// (`ui/feed.md` § Implementation status today: the native arm discards
    /// `body` outright). Mirrors linux's `build_reply_dialog` / web's own
    /// reply overlay.
    OpenReplyDialog(String),
    /// `feed-reply-submit-button` — send the armed [`FeedState::reply_draft`]
    /// through the shared `FeedManager::reply`. No post id of its own: the
    /// draft already carries the target, the same shape `Action::CreateFeed`
    /// takes off `FeedState::form`.
    SubmitReply,
    /// `feed-reply-public-confirm` — flip the armed draft's explicit answer
    /// that this reply goes public under a restricted target the reader
    /// cannot write for (`ui/feed.md` § Encryption at rest → *Ruling 5's
    /// build — the shape*, (e)). Purely local: the answer is read once, at
    /// [`Self::SubmitReply`], and dies with the draft.
    ToggleReplyPublicConfirm,
    /// `feed-post-muted-reveal-button[i]` — un-collapse ONE muted-keyword match
    /// for the rest of the session. The feed twin of
    /// `conversations::Action::RevealMuted`: session-local, never a write to the
    /// sealed list (`moderation.md` § Muted keywords — a hide verb, not a flag).
    RevealMuted(String),
    /// Un-collapse ONE content-policy `collapse` for the rest of the session
    /// (`family-safety.md` § Content policy — "rendered collapsed, reveal
    /// affordance"). Session-local like [`Self::RevealMuted`], and deliberately a
    /// *separate* verb: the floor itself persists, so revealing one post never
    /// relaxes the guardian's policy or the viewer's own threshold. It can never
    /// reveal a `block` — that arm paints no reveal at all.
    RevealContent(String),
    /// `load-remote-content-button` — reveal a post's blocked remote images.
    RevealRemoteImages(String),
    /// Focus reached the last card and the snapshot says there is more.
    LoadMore,
    /// `post-card` click — ui.yaml `feed.sub_pages.post_detail`.
    OpenPostDetail(String),
    /// `gated-post-buy-button` — the self-serve teaser purchase (gap (2c),
    /// `monetization.md` § Per-post pay-to-unlock): subscribe against the
    /// resolved offer's tier, no claim code needed. The money plane's buyer
    /// half, with the teaser that paints it.
    #[cfg(feature = "payments")]
    BuyUnlockOffer(String),
    /// `gated-post-payment-link` — open the offer's external `payment_url` via
    /// the OS default handler (`os_open`, the wizard's provider-link
    /// mechanism); refused for a non-`https` scheme (nest/author-supplied
    /// content — the same anti-phishing-redirect check web/android apply
    /// before opening a payment link).
    #[cfg(feature = "payments")]
    OpenPaymentLink(String),

    // --- the create_feed sub-page ---
    OpenCreateFeed,
    CancelCreateFeed,
    SetRuleType(String),
    ToggleRuleRequired,
    AddRule,
    SetCombination(String),
    SetFactor(String),
    ToggleFactorGlobal,
    AddFactor,
    CreateFeed,

    // --- feed deletion ---
    /// `feed-delete-button` — no confirmation step (the uniform 4-app
    /// pattern: linux/web/android/windows all delete directly).
    DeleteFeed(String),

    // --- the bridge-subscribe form (`feed.md` § Layout & flow region 5) ---
    /// `bridge-feed-subscribe-toggle` — open the inline form, pre-selecting
    /// the first available bridge.
    OpenBridgeForm,
    /// `bridge-form-cancel-button`.
    CancelBridgeForm,
    /// `bridge-form-bridge-select`.
    SetBridgeKind(String),
    /// `bridge-form-subscribe-button`.
    SubscribeBridge,
    /// `bridge-feed-unsubscribe-button[i]`, carrying that row's own id.
    UnsubscribeBridge(i64),
}

/// Which surface of the feed page is showing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    List,
    /// ui.yaml `feed.sub_pages.create_feed`.
    CreateFeed,
    /// ui.yaml `feed.sub_pages.post_detail` — the post id being shown.
    PostDetail(String),
}

/// The create-feed form's local buffers — the only genuinely page-local state
/// on this page. Everything else (compose text/tags, search query, posts) is
/// owned by the manager's snapshot and read back from it.
#[derive(Debug, Default, Clone)]
pub struct CreateFeedForm {
    pub name: String,
    pub rule_type: String,
    pub rule_value: String,
    /// The 0–10 confidence for the label rules (LabelBelow / LabelAbove), which
    /// take a category AND a threshold. Packed with `rule_value` into one wire
    /// value as `"category:threshold"` on Add — the shared encoder splits on
    /// ':' (`libs/fauna-client-feed/src/encoder.rs`), the same shape linux,
    /// windows and android already use. Only the `TextAndNumber` kind reads it.
    pub rule_threshold: String,
    pub rule_required: bool,
    pub combination: String,
    pub factor: String,
    pub factor_weight: String,
    pub factor_global: bool,
    pub rules: Vec<FilterRuleInput>,
    pub factors: Vec<FactorWeightInput>,
}

impl CreateFeedForm {
    fn fresh() -> Self {
        CreateFeedForm {
            rule_type: rule_type_options()[0].value.clone(),
            rule_threshold: fauna_client_feed::DEFAULT_RULE_THRESHOLD.to_string(),
            combination: "all".to_string(),
            // The picker opens on the first shared built-in (`engagement`).
            factor: fauna_client_feed::builtin_factor_options()[0].value.clone(),
            ..Default::default()
        }
    }
}

/// The bridge-subscribe form's local buffers (`feed.md` § Layout & flow
/// region 5) — the `CreateFeedForm` twin: page-local, not manager-owned. The
/// manager's own `BridgeFormState.{bridge_kind,uri,name}` are never written
/// by a setter (only `.error`/`.submitting` are — `subscribe_bridge` takes
/// the three values as plain args), so buffering them here, exactly like
/// linux's dialog buffers its own GTK widgets, is the correct home.
#[derive(Debug, Default, Clone)]
pub struct BridgeSubscribeForm {
    pub kind: String,
    pub uri: String,
    pub name: String,
}

/// Per-hash C2PA-provenance cache — the boolean twin of [`ImageCache`], the
/// same shared bookkeeping over a verdict instead of art. A check that errors
/// folds in as `false` (`Op::run` does that), so `Failed` is never written; it
/// would paint the same "no badge" anyway.
///
/// What a resolved `true` means is [`Op::FetchC2pa`]'s business, and it is
/// **not** the `x-c2pa` header: the entry is written from
/// `detect_c2pa_in_bytes` over the blob's own bytes (`ui/media.md` § C2PA
/// provenance — the uploader's `has_c2pa` is a hint the viewer corrects
/// against ground truth, never a badge in itself).
///
/// The paint gate `c2pa-badge` reads is [`has_c2pa`]: absent and in-flight
/// both read `false`, so the badge never flashes on speculatively.
pub type C2paCache = fauna_core::load_cache::LoadCache<bool>;

/// Whether `hash` has a resolved, positive provenance verdict in `cache`.
fn has_c2pa(cache: &C2paCache, hash: &str) -> bool {
    cache.ready(hash) == Some(&true)
}

/// The feed page's state, hung off [`App`] rather than a process-wide singleton
/// (linux's `OnceLock` shape): the manager is built on auth and dropped on
/// sign-out, so its lifetime is the session's, and `App` already *is* that
/// scope.
#[derive(Default)]
pub struct FeedState {
    /// `None` before the first sign-in — the e2e state serializer runs pre-auth,
    /// so every reader degrades gracefully rather than panicking.
    pub manager: Option<Arc<CliFeedManager>>,
    /// The rail's `DraftsSync`, so `main.rs`'s leave-door flush
    /// (`drafts_autosave::flush_now`) can force a save of `manager`'s current
    /// snapshot at quit time without a debounce wait. `None` before the first
    /// sign-in, or if `drafts::start`'s malformed-secret arm disabled
    /// persistence for this session.
    pub drafts_sync: Option<Arc<drafts::FeedDraftsSync>>,
    /// The HTTP/bulk plane, for blob upload **and** the `post-image` fetch
    /// (`GET /api/v1/blob/<hash>`, the same bulk-binary carve-out). Shares the
    /// `AuthClient`'s single bearer cache rather than minting a parallel token.
    pub content: Option<Arc<dyn NestContentApi>>,
    /// The per-hash `post-image` art cache — the shared spine Media's thumbnails
    /// also hang off ([`ImageCache`]). A feed image's bytes are a *plaintext*
    /// public-post blob fetched raw by hash (no decrypt, unlike a sealed Media
    /// thumbnail); the cache bookkeeping is identical, the byte-source is not.
    pub images: ImageCache,
    /// The `doc-remote-image` art cache, keyed by **url** — a third body-image
    /// byte-source with a third provenance: someone else's host, fetched only
    /// after the reader revealed the post (`crate::remote_image`). Kept apart
    /// from [`Self::images`] because the keys mean different things: a content
    /// hash names bytes that can never change, a url names bytes that can.
    pub remote_images: ImageCache,
    /// `has_c2pa` per post-image hash — the badge's own async check
    /// (`kick_c2pa_fetches` → `Op::FetchC2pa`), independent of [`Self::images`]
    /// because a badge and its art can resolve in either order and neither
    /// gates the other's paint.
    pub c2pa: C2paCache,
    pub mode: Mode,
    /// The attachment's local path, staged at attach and uploaded at submit —
    /// linux's shape. Uploading at attach time would make `compose-file` a
    /// network call on every keystroke of a typed path. What *is* staged at
    /// once is the path's hash-less handle, on the manager ([`set_field`]'s
    /// `ComposeFile` arm): the handle is what a draft carries across a
    /// relaunch, and this path is not.
    pub staged_file: Option<String>,
    pub form: CreateFeedForm,
    /// Posts whose muted-keyword collapse the user opened this session
    /// (`feed-post-muted-reveal-button`) — linux's `REVEALED_MUTED_POSTS`
    /// thread-local, hung off `App` for the same reason the manager is. The
    /// conversations twin is `ConversationsState::revealed_muted`; both are
    /// session-local because revealing one instance must not un-mute the term.
    pub revealed_muted: std::collections::HashSet<String>,
    /// Posts whose **content-policy `collapse`** the user opened this session
    /// (`family-safety.md` § Content policy — "rendered collapsed, reveal
    /// affordance") — linux's `REVEALED_CONTENT_POSTS`. A separate set from
    /// [`Self::revealed_muted`] on purpose: they are different verbs with
    /// different sources (the user's own muted terms vs. a guardian floor or a
    /// spam threshold), so revealing one must not reveal the other on the same
    /// post. Session-local for the same reason, and it can never reveal a
    /// `block` — that arm returns before any reveal is offered.
    pub revealed_content: std::collections::HashSet<String>,
    /// The post whose ⋯ overflow (`feed-post-actions-menu`) is open, by
    /// **post id** rather than row index: a train re-ranks the loaded window in
    /// place, so an index captured at open time can address a different post by
    /// the time the menu's verb is clicked (the apple `open_post_actions`
    /// finding, `topic-factors.md` § Implementation status). At most one menu is
    /// open at a time, which is what lets the shared action read the verbs
    /// unscoped.
    pub actions_open: Option<String>,
    /// The post whose ⋯ menu is showing the **delete confirm step**, by post id
    /// for [`Self::actions_open`]'s reason.
    ///
    /// Delete is the one destructive verb in this menu and the only one that
    /// keeps a confirm (`feed.md` § State & data shape → *Post deletion*);
    /// unpublish deliberately has none, being idempotent and reversible. The
    /// two-step lives inside the same menu — linux's `post_list.rs` shape,
    /// which in turn mirrors conversations'
    /// `dm-message-delete-button`/`-confirm-button` verbatim.
    ///
    /// Kept beside `actions_open` rather than folded into it because they
    /// answer different questions ("which menu is open" vs "is that menu armed
    /// to destroy"), and every path that closes the menu clears both — an armed
    /// confirm surviving a menu close would re-arm on the next open.
    pub delete_confirm: Option<String>,
    /// What the ⋯ menu's last copy affordance put on the clipboard, and which
    /// post's which verb produced it — painted back as that button's `copied`
    /// attr and as a visible line.
    ///
    /// Keyed by **post id**, not row index, for [`Self::actions_open`]'s reason:
    /// the loaded window re-ranks in place, so an index would repaint the copied
    /// value onto whichever post slid into that slot.
    pub web_copied: Option<CopiedFeedLink>,
    /// Whether the rich compose dialog (`feed-compose-dialog`) is open.
    ///
    /// While it is, the composer's elements paint **inside** the dialog rather
    /// than in the inline bar — one `compose-text-field` at any instant, never
    /// two. (GTK can afford both: linux's dialog is a separate `adw::Window`
    /// over a still-mapped bar. An immediate-mode terminal paints one element
    /// list, so a duplicated id would be a duplicate registry entry.)
    pub compose_dialog_open: bool,
    /// The blob hash whose `image-lightbox` is open, if any — set by a
    /// `post-image` click, cleared by Escape.
    pub lightbox: Option<String>,
    /// The post whose `post-tip-list` attribution window is open, if any — set
    /// by `post-tip-list-button`, cleared by Escape. Keyed by post id, not row
    /// index, for the `OpenPostActions` reason: the loaded window re-ranks in
    /// place, so an index would open the wrong post's tips after a re-rank.
    #[cfg(feature = "payments")]
    pub tip_list_open: Option<String>,
    /// Whether the inline bridge-subscribe form (`bridge-form-*`, `feed.md` §
    /// Layout & flow region 5) is open.
    pub bridge_form_open: bool,
    /// The bridge-subscribe form's local buffers.
    pub bridge_form: BridgeSubscribeForm,
    /// The reply-compose dialog (`feed-reply-dialog`), armed by
    /// `feed-reply-button` — `None` unless open. Page-local like
    /// [`Self::bridge_form`]/[`Self::form`], not manager-owned: unlike
    /// `compose`, the shared manager holds no reply-in-progress state.
    pub reply_draft: Option<ReplyDraft>,
    /// The engagement-cue capture over this manager's post list
    /// ([`cues`]) — built with the manager, so a re-auth that replaces one
    /// replaces both and a tracker can never sample into another actor's
    /// engine.
    pub cue_capture: cues::CueCapture,
}

/// The armed reply dialog's own buffer — target post id + typed body.
#[derive(Debug, Default, Clone)]
pub struct ReplyDraft {
    pub post_id: String,
    pub text: String,
    /// `feed-reply-public-confirm` — the explicit per-reply answer that the
    /// words go public, taken only under a restricted target this reader
    /// cannot write for (`PostSummary::reply_audience` says so). `false` at
    /// every open, never remembered: the dialog offers it, the manager's
    /// `reply_public_confirmed` acts on it.
    pub public_confirmed: bool,
}

impl FeedState {
    pub fn snapshot(&self) -> Option<FeedSnapshot> {
        self.manager.as_ref().map(|m| m.snapshot())
    }
}

/// Forward manager notifications into the render loop's `UiMessage` channel.
///
/// `on_changed` fires **synchronously on whatever thread mutated** (a tokio
/// worker for the async methods), so it must not touch `App`. `try_send`-style
/// forwarding + a fresh snapshot read per tick makes coalescing safe: the
/// receiver always reads current state, so a dropped duplicate loses nothing.
struct TuiFeedObserver {
    tx: UnboundedSender<UiMessage>,
}

impl FeedSnapshotObserver for TuiFeedObserver {
    fn on_changed(&self) {
        // A closed channel means the app is shutting down — nothing to notify.
        let _ = self.tx.send(UiMessage::Data(DataMessage::FeedChanged));
    }
}

/// Build the feed manager over the authed transport + the local actor's signing
/// secret, attach the observer, and kick the first load.
///
/// Called from the one post-auth hook (`session::establish`), so every path that
/// produces a session — onboarding, the launch router, the e2e patch — gets a
/// feed without each remembering to wire one.
pub fn init(
    nest: Arc<NestClient>,
    content: Arc<dyn NestContentApi>,
    secret: [u8; 32],
    period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
    preferences: fauna_client_config::SharedPreferenceStore,
    tx: &UnboundedSender<UiMessage>,
    session_generation: u64,
) -> FeedState {
    let manager = Arc::new(FeedManager::new(nest, secret));
    manager.set_period_key_store(period_keys);
    manager.set_preference_store(preferences);
    manager.add_observer(Arc::new(TuiFeedObserver { tx: tx.clone() }));

    let m = Arc::clone(&manager);
    tokio::spawn(async move {
        m.refresh_feeds().await;
        // The bridge-feed selector list + the `bridge-feed-subscribe-toggle`
        // gate (`version-compatibility.md` § Dim 3) are separate nest tables
        // from the custom-feed list, so they need their own refresh —
        // linux's `connect_map` becomes-visible hook fires all three the
        // same way (`views/feed/mod.rs:201-204`).
        m.refresh_bridge_feeds().await;
        m.refresh_available_bridges().await;
        // `refresh_feeds` only lists the actor's custom feeds — it never
        // queries posts (a fresh actor has none, so nothing else runs it,
        // per its own doc comment). The Feed page has no poll backstop and
        // otherwise relies entirely on `nav_enter_op`'s `RefreshCurrentFeed`,
        // which fires only on a *page edge* (`app.page` changing) — but
        // `Page::Feed` is `App`'s own default (`Page::ALL[0]`), so a session
        // that establishes while `app.page` is already `Feed` (every fresh
        // login/switch: onboarding, the launch router's `Online` path, the
        // e2e patch) sees no edge and never fires it. linux's twin fires on
        // its widget's `connect_map`, a real becomes-visible signal that
        // covers the first show too; a fresh manager here is tui's equivalent
        // moment, so kick the same reload this constructor's own doc comment
        // already promises ("kick the first load").
        m.refresh_current_feed().await;
    });

    // Fetch-on-session-start for the engagement-cue capture
    // (`engagement-cues.md` § At rest): install the sealed `cues:v1` rollup
    // before this device's observations can put one — the manager suppresses
    // puts until it runs, because a rollup folded fresh here would overwrite
    // every cue the user's other devices recorded. An unopenable rollup lands on
    // the page's error line, never a silent fresh start. Retried as linux's is:
    // it is one RPC plus a local unseal, which the transport's own
    // park-until-connected does not cover. Its own task, so it never delays the
    // first feed load above.
    let m = Arc::clone(&manager);
    let hydrate_tx = tx.clone();
    tokio::spawn(async move {
        let hydrated = fauna_sleep::retry(11, std::time::Duration::from_millis(500), || {
            m.hydrate_cues()
        })
        .await;
        if let Err(message) = hydrated {
            let _ = hydrate_tx.send(UiMessage::Data(DataMessage::Page(
                session_generation,
                crate::app::PageOutcome::Feed(Outcome::Error(message)),
            )));
        }
        // The Layer-B signal-sharing opt-in, beside it: the producer inside
        // `record_observation` must respect a persisted opt-in from the first
        // exposure, not only once the user opens Personalization. Best-effort —
        // it defaults off, so a failed read leaves the producer silent.
        let _ = m.hydrate_signal_optin().await;
    });

    FeedState {
        manager: Some(manager),
        drafts_sync: None,
        content: Some(content),
        images: ImageCache::new(),
        remote_images: ImageCache::new(),
        c2pa: C2paCache::default(),
        mode: Mode::default(),
        staged_file: None,
        form: CreateFeedForm::fresh(),
        revealed_muted: std::collections::HashSet::new(),
        revealed_content: std::collections::HashSet::new(),
        actions_open: None,
        delete_confirm: None,
        web_copied: None,
        compose_dialog_open: false,
        lightbox: None,
        #[cfg(feature = "payments")]
        tip_list_open: None,
        bridge_form_open: false,
        bridge_form: BridgeSubscribeForm::default(),
        reply_draft: None,
        cue_capture: cues::CueCapture::default(),
    }
}

// ── Field access ────────────────────────────────────────────────────────────

/// A feed-page editable field (`crate::feed`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FeedField {
    /// The composer. These three read back off the manager's snapshot, not a
    /// local buffer — the manager owns compose state, so a bridge-driven
    /// `update_compose` shows up in the input exactly as a keystroke would.
    ComposeText,
    ComposeTags,
    /// A **path**, not an OS file picker (`tui.md` § Declared platform
    /// absences 4). Staged locally and uploaded at submit; a path naming a
    /// regular file also stages its hash-less handle on the manager at once.
    ComposeFile,
    /// `compose-gate-preview-field` — the public teaser of a gated post.
    /// Rendered only while the post is gated (a tier is selected, or sell mode
    /// is on); both gated submits validate it non-empty.
    ComposeGatePreview,
    /// `compose-sell-price` — free text, becomes the auto-minted unlock tier's
    /// `price_hint` (`monetization.md` § Per-post pay-to-unlock). The money
    /// plane's, with the sell composer that paints it.
    #[cfg(feature = "payments")]
    ComposeSellPrice,
    /// `compose-sell-asking-price` — the machine-comparable sats price for the
    /// auto-minted unlock tier, independent of `ComposeSellPrice`
    /// (`monetization.md` § The asking price).
    #[cfg(feature = "payments")]
    ComposeSellAskingPrice,
    /// A **re-query**, not a local filter (`feed.md` § Anti-patterns) — writing
    /// it costs a round trip, which is why the write path can return work.
    Search,
    CreateFeedName,
    RuleValue,
    /// `feed-rule-threshold-input` — only the label rules (LabelBelow /
    /// LabelAbove) render it; see [`CreateFeedForm::rule_threshold`].
    RuleThreshold,
    FactorWeight,
    /// `bridge-form-uri-input`.
    BridgeUri,
    /// `bridge-form-name-input`.
    BridgeName,
    /// `feed-reply-text-field` — the armed [`FeedState::reply_draft`]'s body.
    /// Page-local like the other dialog/form buffers above: the manager owns
    /// no reply-in-progress state, unlike the compose text it superficially
    /// resembles.
    ReplyText,
}

/// Read one of the page's editable fields.
///
/// Compose text/tags and the search query read back off the **snapshot**, not a
/// local buffer: the manager owns them, and a bridge-driven `update_compose`
/// must show up in the input exactly as a keystroke would (the same discipline
/// the wizard's `WizardField::Handle` follows against the onboarding machine).
pub fn field(state: &FeedState, field: &FeedField) -> String {
    let snap = state.snapshot();
    let compose = snap.as_ref().map(|s| &s.compose);
    match field {
        FeedField::ComposeText => compose.map(|c| c.text.clone()).unwrap_or_default(),
        FeedField::ComposeTags => compose.map(|c| c.tags.clone()).unwrap_or_default(),
        FeedField::ComposeFile => state.staged_file.clone().unwrap_or_default(),
        FeedField::ComposeGatePreview => {
            compose.map(|c| c.gate_preview.clone()).unwrap_or_default()
        }
        #[cfg(feature = "payments")]
        FeedField::ComposeSellPrice => compose
            .and_then(|c| c.sell.as_ref().map(|s| s.price.clone()))
            .unwrap_or_default(),
        #[cfg(feature = "payments")]
        FeedField::ComposeSellAskingPrice => compose
            .and_then(|c| c.sell.as_ref().map(|s| s.asking_price.clone()))
            .unwrap_or_default(),
        FeedField::Search => snap
            .as_ref()
            .and_then(|s| s.search_query.clone())
            .unwrap_or_default(),
        FeedField::CreateFeedName => state.form.name.clone(),
        FeedField::RuleValue => state.form.rule_value.clone(),
        FeedField::RuleThreshold => state.form.rule_threshold.clone(),
        FeedField::FactorWeight => state.form.factor_weight.clone(),
        FeedField::BridgeUri => state.bridge_form.uri.clone(),
        FeedField::BridgeName => state.bridge_form.name.clone(),
        FeedField::ReplyText => state
            .reply_draft
            .as_ref()
            .map(|d| d.text.clone())
            .unwrap_or_default(),
    }
}

/// Write one of the page's editable fields.
///
/// Returns a future's worth of work for the caller to run when the write needs
/// the network: `set_search_query` is a **re-query** (`feed.md` § Anti-patterns:
/// the client never filters the loaded list), so it cannot be applied
/// synchronously. Everything else lands in place and returns `None`.
/// The manager `?` sits inside the arms that genuinely need it, not at the top
/// of the fn: the composer and the search re-query live on the manager's
/// snapshot, but `staged_file` and the `create_feed` form are plain local
/// buffers. Gating all seven on the manager silently dropped a local write when
/// none was installed — the same "no error, no effect" shape the exhaustive
/// `Field` nesting exists to retire (`apps/tui.md` § Target state).
pub fn set_field(state: &mut FeedState, field: FeedField, value: String) -> Option<PendingSearch> {
    match field {
        FeedField::ComposeText => {
            let manager = state.manager.clone()?;
            let snap = manager.snapshot();
            manager.update_compose(value, snap.compose.tags, snap.compose.attached_file);
        }
        FeedField::ComposeTags => {
            let manager = state.manager.clone()?;
            let snap = manager.snapshot();
            manager.update_compose(snap.compose.text, value, snap.compose.attached_file);
        }
        // The upload happens at submit (see `staged_file`), but the pick's
        // hash-less handle goes to the manager now, so a draft saved before the
        // submit carries the file by name (`feed.md` § Persistence →
        // *Attachments by content address*). The field drives that handle only
        // while it holds a path: a restored draft's handle has no path behind
        // it, so a fresh pick replaces it or `compose-file-remove` drops it —
        // never an edit of an empty field that does not show it.
        FeedField::ComposeFile => {
            let had_path = state.staged_file.is_some();
            state.staged_file = (!value.is_empty()).then_some(value);
            let handle = state.staged_file.as_deref().and_then(picked_handle);
            if let Some(manager) = state.manager.clone()
                && (handle.is_some() || had_path)
            {
                let snap = manager.snapshot();
                manager.update_compose(snap.compose.text, snap.compose.tags, handle);
            }
        }
        // The teaser is shared by every restricted answer (a tier, a sale, a
        // room), so it is staged alone — writing it must never flip the gate
        // select the user chose.
        FeedField::ComposeGatePreview => {
            state.manager.clone()?.update_compose_preview(value);
        }
        #[cfg(feature = "payments")]
        FeedField::ComposeSellPrice => {
            let manager = state.manager.clone()?;
            let snap = manager.snapshot();
            // Only meaningful in sell mode; outside it there is nothing to
            // stage the price onto (and staging it would be the "price lingers
            // from a sell the author backed out of" state `sell` exists to
            // make unrepresentable).
            let mut sell = snap.compose.sell?;
            sell.price = value;
            manager.update_compose_sell(Some(sell), snap.compose.gate_preview);
        }
        #[cfg(feature = "payments")]
        FeedField::ComposeSellAskingPrice => {
            let manager = state.manager.clone()?;
            let snap = manager.snapshot();
            let mut sell = snap.compose.sell?;
            sell.asking_price = value;
            manager.update_compose_sell(Some(sell), snap.compose.gate_preview);
        }
        FeedField::Search => {
            return Some(PendingSearch {
                manager: state.manager.clone()?,
                query: (!value.is_empty()).then_some(value),
            });
        }
        FeedField::CreateFeedName => state.form.name = value,
        FeedField::RuleValue => state.form.rule_value = value,
        FeedField::RuleThreshold => state.form.rule_threshold = value,
        FeedField::FactorWeight => state.form.factor_weight = value,
        FeedField::BridgeUri => state.bridge_form.uri = value,
        FeedField::BridgeName => state.bridge_form.name = value,
        // A stray write while the dialog isn't open (already dismissed, or
        // never armed) is a no-op — there is no draft to write into.
        FeedField::ReplyText => {
            if let Some(draft) = state.reply_draft.as_mut() {
                draft.text = value;
            }
        }
    }
    None
}

/// A search re-query the caller must drive (awaited on the agent's path so the
/// driver's single-shot read sees the filtered list; spawned on the keyboard's
/// so a slow nest never freezes the render loop).
pub struct PendingSearch {
    manager: Arc<CliFeedManager>,
    query: Option<String>,
}

impl PendingSearch {
    pub async fn run(self) {
        self.manager.set_search_query(self.query).await;
    }
}

/// Re-run the current feed query on entering the Feed tab — the page's leg of
/// the one nav-edge hook (`crate::app::on_nav_enter`).
///
/// The manager is observer-backed, so the *posts* arrive without this. What does
/// **not** arrive is the **sealed scorer** set — the user's muted keywords and
/// trained topic factors are loaded inside the manager's `reload`, and nowhere
/// else. So editing the muted list on the `muted-words` Settings sub-page and
/// walking back to the feed left tui rendering against the pre-edit scorers:
/// content the user had just muted stayed visible, with no error and nothing to
/// retry. (iOS shipped the same bug — `test_feed_muted_posts.py`'s  note.)
///
/// Awaited by the agent like every other nav-edge op, so a driver's next read
/// never sees the pre-refresh frame.
pub fn nav_enter_op(state: &FeedState) -> Option<Op> {
    Some(Op::RefreshCurrentFeed {
        manager: state.manager.clone()?,
    })
}

// ── Gesture dispatch ────────────────────────────────────────────────────────

/// Apply a feed gesture's **local** half and hand back its network half, if any.
///
/// The split exists because the two callers need different things. The agent's
/// click path must **await** the network op (element reads are single-shot, so a
/// submit must have landed and the feed reloaded before `/element/click`
/// replies); the keyboard path must **spawn** it (the render loop can never
/// block on a nest). A single `async fn(&mut App)` could serve only the first —
/// `&mut App` cannot cross a `tokio::spawn`. So local state changes land here,
/// synchronously, and everything that touches the network comes back as an
/// [`Op`] owning only `Arc`s.
///
/// This is the same shape `App::dispatch_wizard` already uses: validate and
/// mutate locally, then hand the caller a runnable that owns the machine.
impl Action {
    /// The wire kind this gesture issues — the offline gate's input
    /// (`crate::element::Gesture::wire_kind`). Exhaustive with no fallback
    /// arm, so a new feed gesture must answer the offline question.
    ///
    /// Note how few arms desensitize: the feed is mostly `OfflineSafe` /
    /// `OfflineQueued` by design (a post's id is `blake3(body)`, an interaction
    /// queues), so composing, liking, training and managing feeds all keep
    /// working with no nest. That is the classification doing its job — this
    /// function only reports what each gesture calls.
    pub fn wire_kind(&self) -> Option<&'static str> {
        match self {
            // `FeedManager::submit_post` — content-addressed id, applied
            // locally and synced as data.
            Action::SubmitPost => Some("fauna.posts.create"),
            Action::Interact { .. } => Some("fauna.posts.interact"),
            // Reply is COMPOSED, not recorded — the same door `SubmitPost`
            // uses (`FeedManager::reply` → `compose_referencing_post` →
            // `PostsClient::posts_create`), never `fauna.posts.interact`.
            Action::SubmitReply => Some("fauna.posts.create"),
            // `train_post`/`untrain_post` read the model, then put it back; the
            // put is the mutation.
            Action::TrainPost { .. } => Some("fauna.personalization.model.put"),
            Action::CreateFeed => Some("fauna.feed.create"),
            Action::DeleteFeed(_) => Some("fauna.feed.delete"),
            Action::SubscribeBridge => Some("fauna.bridges.feeds.create"),
            Action::UnsubscribeBridge(_) => Some("fauna.bridges.feeds.delete"),
            // The self-serve teaser purchase — `subscribe_publishing_ek` over
            // `fauna.subscriptions.subscribe`. Online-only, so this is the
            // page's one gesture that desensitizes without a nest.
            #[cfg(feature = "payments")]
            Action::BuyUnlockOffer(_) => Some("fauna.subscriptions.subscribe"),
            // The own-post web-publishing verbs. Publish/unpublish are
            // classified `OfflineSafe` (`offline_class`) — a publish row is
            // per-actor state that syncs, so the gate leaves them actuable with
            // no nest; only the MINT is `OnlineOnly`, because a capability
            // token is signed by the nest's holder identity and cannot be
            // produced locally.
            Action::PublishWeb(_) => Some("fauna.web.publish.set"),
            Action::UnpublishWeb(_) => Some("fauna.web.publish.unset"),
            Action::CopyPaywallLink(_) => Some("fauna.web.paywall.mint_token"),
            // Destroying an own post is classified `OfflineSafe`: the author
            // signs a `Tombstone` locally and the end state is idempotent
            // (`deleted:false` is a success), so the gate leaves it actuable
            // with no nest. Only the CONFIRM declares it — arming the step
            // issues nothing, so gating the arm would grey the door to a verb
            // that works.
            Action::ConfirmDeletePost(_) => Some("fauna.posts.delete"),

            // Local. Three groups, none of which issues a mutation:
            //   * view switches whose refetch is a `Read`
            //     (`fauna.feed.posts` and friends), which the gate declines to
            //     decide on by construction;
            //   * overlays, reveals and the create-feed / bridge-subscribe form
            //     buffers — snapshot or `App` writes only;
            //   * `OpenPaymentLink`, which hands a URL to the OS.
            // The two money-plane gestures take their own arm so the flavor
            // that has no such variants compiles the list unchanged.
            #[cfg(feature = "payments")]
            Action::ToggleSellSubscribersFree | Action::OpenPaymentLink(_) => None,
            Action::SelectFeed(_)
            | Action::SelectTrending
            | Action::ClearSearch
            | Action::LoadMore
            | Action::OpenPostDetail(_)
            | Action::OpenPostActions(_)
            | Action::StartDeletePost(_)
            | Action::OpenComposeDialog
            | Action::OpenReplyDialog(_)
            | Action::ToggleReplyPublicConfirm
            | Action::OpenLightbox(_)
            | Action::SetGateTier(_)
            | Action::RemoveComposeAttachment
            | Action::RevealMuted(_)
            | Action::RevealContent(_)
            | Action::RevealRemoteImages(_)
            // Purely local: the origin and the slug are both already resolved
            // on screen, so copying the public page URL costs no round trip.
            // Its sibling `CopyPaywallLink` is NOT here — that one mints.
            | Action::CopyWebLink(_)
            | Action::OpenCreateFeed
            | Action::CancelCreateFeed
            | Action::SetRuleType(_)
            | Action::ToggleRuleRequired
            | Action::AddRule
            | Action::SetCombination(_)
            | Action::SetFactor(_)
            | Action::ToggleFactorGlobal
            | Action::AddFactor
            | Action::OpenBridgeForm
            | Action::CancelBridgeForm
            | Action::SetBridgeKind(_) => None,
            // Local, same as the overlay group above — it is only spelled as
            // its own arm because `#[cfg]` cannot sit on one alternative of an
            // or-pattern.
            #[cfg(feature = "payments")]
            Action::OpenTipList(_) => None,
        }
    }
}

pub fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    let manager = app.feed.manager.clone()?;
    match action {
        // --- network ---
        Action::SelectFeed(id) => {
            // Switching feeds changes the composition, and with it the verbs'
            // train target — an overlay opened against the old one must not
            // survive into the new feed's rows.
            app.feed.actions_open = None;
            // Same reasoning for the tip window: it is opened against ONE
            // post, and the new feed's rows are different posts.
            #[cfg(feature = "payments")]
            {
                app.feed.tip_list_open = None;
            }
            Some(Op::SelectFeed { manager, id })
        }
        // Trending is a *composition* switch like any other feed selection, so it
        // closes a stale overflow for the same reason `SelectFeed` does.
        Action::SelectTrending => {
            app.feed.actions_open = None;
            #[cfg(feature = "payments")]
            {
                app.feed.tip_list_open = None;
            }
            Some(Op::SelectTrending { manager })
        }
        // ── the own-post web-publishing verbs ──
        //
        // Each arm re-checks the gate the paint applied rather than trusting it:
        // the loaded window re-ranks in place, so a verb clicked a frame late
        // must resolve against the post it names, not the one that was there.
        // ── own-post delete (`feed.md` § State & data shape → Post deletion) ──
        //
        // Two steps in the same menu, and the same re-resolve-at-click rule as
        // the web verbs: the loaded window re-ranks in place, so both arms
        // re-check ownership against the post they NAME rather than trusting
        // the paint that produced them.
        Action::StartDeletePost(post_id) => {
            own_post(app, &post_id)?;
            app.feed.delete_confirm = Some(post_id);
            None
        }
        Action::ConfirmDeletePost(post_id) => {
            own_post(app, &post_id)?;
            // Both close on dispatch: the card is about to go, so leaving the
            // menu open would leave it anchored to a post that no longer
            // exists (conversations' `ConfirmDeleteMessage` takes its overlay
            // the same way).
            app.feed.delete_confirm = None;
            app.feed.actions_open = None;
            Some(Op::DeletePost { manager, post_id })
        }
        Action::PublishWeb(post_id) => {
            own_published_post(app, &post_id, false)?;
            Some(Op::WebPublish {
                nest: app.session.as_ref()?.client.clone(),
                manager,
                post_id,
            })
        }
        Action::UnpublishWeb(post_id) => {
            own_published_post(app, &post_id, true)?;
            Some(Op::WebUnpublish {
                nest: app.session.as_ref()?.client.clone(),
                manager,
                post_id,
            })
        }
        // Purely local — the origin and the slug are both already resolved on
        // screen, so the public link costs no round trip. Nothing is copied
        // when there is no serving origin: the button paints disabled in that
        // case, and this arm refuses independently rather than trusting the
        // paint (a dead link on the clipboard is worse than no copy).
        Action::CopyWebLink(post_id) => {
            let slug = own_published_post(app, &post_id, true)?.web_slug.clone()?;
            let origin = crate::settings::web::site_link(&app.settings).origin?;
            let url = fauna_client_web::post_page_url(&origin, &slug);
            crate::wizard::copy_to_clipboard(&url);
            app.feed.web_copied = Some(CopiedFeedLink {
                post_id,
                kind: CopiedKind::Web,
                url,
            });
            None
        }
        // A fresh mint per click: the token is short-lived by ratified design
        // (`monetization.md` § Pillar 2 → Creator comp-link surface) and
        // re-minting is free, so re-copying always yields a link that works from
        // now, never a cached one that already expired.
        Action::CopyPaywallLink(post_id) => {
            let post = own_published_post(app, &post_id, true)?;
            // Gated posts only — the button paints only for them, and this
            // refusal is what keeps a re-ranked window from minting against an
            // ungated post.
            post.gated_tier.as_ref()?;
            let slug = post.web_slug.clone()?;
            let origin = crate::settings::web::site_link(&app.settings).origin?;
            Some(Op::WebMintPaywallLink {
                nest: app.session.as_ref()?.client.clone(),
                post_id,
                slug,
                origin,
            })
        }
        Action::ClearSearch => Some(Op::ClearSearch { manager }),
        Action::LoadMore => Some(Op::LoadMore { manager }),
        Action::SubmitPost => {
            // Posting closes the rich dialog, exactly as linux's `d.close()` on
            // its dialog's Post. Nothing is lost if the submit is rejected: the
            // compose text and `compose-error` are manager-owned snapshot state,
            // so a validation failure re-appears on the inline bar the dialog
            // was covering.
            app.feed.compose_dialog_open = false;
            Some(Op::SubmitPost {
                manager,
                content: app.feed.content.clone(),
                staged: app.feed.staged_file.take(),
            })
        }
        Action::Interact { post_id, action } => {
            // Through the manager, never a bare `PostsClient::posts_interact`:
            // the count on the button renders from the snapshot, and the manager
            // is what writes the nest's post-act counters back into it
            // (`FeedManager::interact`). The old direct call is exactly why a
            // tapped ♥ stayed at 0 here.
            let manager = app.feed.manager.clone()?;
            Some(Op::Interact {
                manager,
                post_id,
                action,
            })
        }
        // --- the reply-compose dialog (`feed-reply-dialog`) ---
        Action::OpenReplyDialog(post_id) => {
            app.feed.reply_draft = Some(ReplyDraft {
                post_id,
                text: String::new(),
                // Unchecked at every open, never remembered (ui.yaml's
                // `feed-reply-public-confirm`).
                public_confirmed: false,
            });
            // A transient overlay doesn't survive another one opening over it —
            // the `OpenComposeDialog`/`OpenLightbox` posture.
            app.feed.actions_open = None;
            None
        }
        Action::ToggleReplyPublicConfirm => {
            // The answer can only be given where the dialog asks it — under a
            // target this reader cannot write for. Elsewhere the flip is a
            // no-op, so no stray gesture marks a sealable reply public.
            let asked = app.feed.reply_draft.as_ref().is_some_and(|d| {
                manager
                    .snapshot()
                    .find_post(&d.post_id)
                    .and_then(|p| p.reply_audience)
                    == Some(ReplyAudience::PublicByConfirmation)
            });
            if asked && let Some(draft) = app.feed.reply_draft.as_mut() {
                draft.public_confirmed = !draft.public_confirmed;
            }
            None
        }
        Action::SubmitReply => {
            let draft = app.feed.reply_draft.take()?;
            // The button paints disabled on empty text (mirrors linux's
            // `build_reply_dialog` guard); this refuses independently rather
            // than trusting the paint — an empty reply is meaningless, and
            // `FeedManager::reply` itself would refuse it, but restoring the
            // draft here keeps a stray Enter from silently dropping typed text.
            if draft.text.trim().is_empty() {
                app.feed.reply_draft = Some(draft);
                return None;
            }
            // Closes optimistically, exactly like `SubmitPost` above: a
            // failure lands on the page's `error-message`, not a slot inside
            // the (now-closed) dialog — the same tradeoff linux's `d.close()`
            // and web's `closeReplyDialog()` both make.
            Some(Op::Reply {
                manager,
                content: app.feed.content.clone(),
                post_id: draft.post_id,
                body: draft.text,
                public_confirmed: draft.public_confirmed,
            })
        }
        Action::CreateFeed => {
            let form = app.feed.form.clone();
            // Close the sub-page optimistically: `create_feed` refreshes the feed
            // list itself, so a success needs no further local step, and a
            // failure surfaces on `error-message` over the list.
            app.feed.mode = Mode::List;
            app.feed.form = CreateFeedForm::fresh();
            Some(Op::CreateFeed { manager, form })
        }
        Action::DeleteFeed(feed_id) => Some(Op::DeleteFeed { manager, feed_id }),

        // --- local: the gate select + its sell controls ---
        //
        // All three answers land through the shared setters, which clear each
        // other — so "gated to a tier AND selling" never becomes representable
        // here (`FeedComposeState::sell`).
        Action::SetGateTier(label) => {
            let snap = manager.snapshot();
            let preview = snap.compose.gate_preview;
            // "Sell this post…" is the paywall-designation gesture — the money
            // plane's author half (`dynamic-features.md` § Platform-family
            // surface excision → *The price-and-route class*). A store-safe
            // build never offers the answer (`compose_elements`) and never
            // takes it: the label falls through to the tier arm, which is
            // what an unknown select value already does.
            #[cfg(feature = "payments")]
            let is_sell = label == feed::post::GATE_SELL;
            #[cfg(not(feature = "payments"))]
            let is_sell = false;
            if is_sell {
                // Entering sell mode takes the defaults — notably
                // `subscribers_get_it_free: true` (user-ratified 2026-07-29).
                #[cfg(feature = "payments")]
                manager.update_compose_sell(Some(SellComposeState::default()), preview);
            } else if label == feed::post::GATE_PUBLIC {
                manager.update_compose_gate(None, preview);
            } else if let Some(room) = snap
                .own_rooms
                .iter()
                .find(|r| feed::post::gate_room(&r.label) == label)
            {
                // A room answer: the post is room-restricted, sealed so only
                // that room's floor members open it.
                manager.update_compose_room(Some(room.room.clone()), preview);
            } else {
                manager.update_compose_gate(Some(label), preview);
            }
            None
        }
        #[cfg(feature = "payments")]
        Action::ToggleSellSubscribersFree => {
            let snap = manager.snapshot();
            let mut sell = snap.compose.sell?;
            sell.subscribers_get_it_free = !sell.subscribers_get_it_free;
            manager.update_compose_sell(Some(sell), snap.compose.gate_preview);
            None
        }
        Action::RemoveComposeAttachment => {
            app.feed.staged_file = None;
            let snap = manager.snapshot();
            manager.update_compose(snap.compose.text, snap.compose.tags, None);
            None
        }

        // --- local: reveal is a snapshot mutation, not a fetch (the bytes are
        // already there; the block was merely marked blocked) ---
        Action::RevealRemoteImages(post_id) => {
            manager.reveal_remote_images(post_id);
            None
        }

        // --- local: same reasoning — the body is already in the snapshot, the
        // collapse was only a render decision over `FeedManager::is_muted` ---
        Action::RevealMuted(post_id) => {
            app.feed.revealed_muted.insert(post_id);
            None
        }

        // --- local, same reasoning again: the content-policy collapse is a
        // render decision over the shared verdict, so revealing is one set
        // insert and never a policy write ---
        Action::RevealContent(post_id) => {
            app.feed.revealed_content.insert(post_id);
            None
        }

        // --- purely local surfaces: the rich compose dialog and the lightbox ---
        Action::OpenComposeDialog => {
            app.feed.compose_dialog_open = true;
            // A transient overlay doesn't survive another one opening over it —
            // the `OpenPostDetail`/`OpenCreateFeed` posture.
            app.feed.actions_open = None;
            None
        }
        Action::OpenLightbox(hash) => {
            app.feed.lightbox = Some(hash);
            app.feed.actions_open = None;
            None
        }
        #[cfg(feature = "payments")]
        Action::OpenTipList(post_id) => {
            app.feed.tip_list_open = Some(post_id);
            app.feed.actions_open = None;
            None
        }

        // --- opening the post-detail sub-page: local for a public post the
        // timeline already holds, a fetch for a DEEP LINK the feed never loaded,
        // and a sealed-body unseal on top for a GATED one ---
        Action::OpenPostDetail(post_id) => {
            app.feed.mode = Mode::PostDetail(post_id.clone());
            // A transient overlay doesn't follow you off the list — the
            // `conversations` posture (its ⋯ overlay clears on thread change);
            // there is no ui.yaml close affordance on either surface.
            app.feed.actions_open = None;
            // Both round trips ride ONE op rather than two actions, because they
            // are ordered: a post outside the timeline must be fetched before
            // anything can ask whether it is gated. The op resolves first, then
            // consults the snapshot — so a card click on a public post still
            // costs nothing, and neither leg needs the caller to know which case
            // it is in (`fauna_feed::FeedManager::resolve_post` is a no-op for a
            // loaded post; the unseal is skipped for a post that isn't gated or
            // was already unsealed this session).
            Some(Op::OpenPostDetail {
                manager,
                content: app.feed.content.clone(),
                post_id,
            })
        }

        // --- the self-serve teaser purchase (gap (2c)) ---
        #[cfg(feature = "payments")]
        Action::BuyUnlockOffer(post_id) => Some(Op::BuyUnlockOffer { manager, post_id }),
        // --- local: hand the payment URL to the OS default handler
        // (`os_open`, the wizard's provider-link mechanism); refused for a
        // non-https scheme via the shared `fauna_core::subscription::
        // is_safe_payment_url` guard (F-CL2 anti-phishing-redirect class) ---
        #[cfg(feature = "payments")]
        Action::OpenPaymentLink(url) => {
            if fauna_core::subscription::is_safe_payment_url(&url) {
                crate::os_open::open(&url);
            } else {
                app.errors.insert(
                    crate::pages::Page::Feed,
                    subscriptions::UNSAFE_PAYMENT_URL.to_string(),
                );
            }
            None
        }

        // --- local: the post-card ⋯ overflow ---
        Action::OpenPostActions(post_id) => {
            let own = app
                .session
                .as_ref()
                .zip(app.feed.manager.as_ref())
                .and_then(|(s, m)| {
                    m.snapshot()
                        .find_post(&post_id)
                        .map(|p| p.author == s.actor_id)
                })
                .unwrap_or(false);
            app.feed.actions_open = Some(post_id);
            // A fresh open is never pre-armed: `delete_confirm` is a
            // property of THIS opening of the menu, so arming, closing and
            // reopening must present the un-armed verb again.
            app.feed.delete_confirm = None;
            // The menu about to paint carries this actor's web-publishing verbs,
            // whose copy affordances need an origin. `view` is the settings
            // page's own hydrated flag; `None` means nobody has read those
            // inputs yet, and disabling a copy verb off unread state would tell
            // a creator with a working address that they have none.
            //
            // Own posts only, and once: a stranger's menu has no such verbs, and
            // a hydrated `view` is never re-read here (the settings page owns
            // refresh).
            if own && app.settings.web.view.is_none() {
                let nest = app.session.as_ref()?.client.clone();
                let handle = app.settings.web_origin_handle();
                return Some(Op::HydrateWebOrigin { nest, handle });
            }
            None
        }
        // Train-in-context: the verb targets the composed feed's single trained
        // factor. With none resolved the menu shows `feed-post-train-target-sheet`
        // instead and paints no verbs, so this arm cannot be reached without one
        // — but it is still gated rather than unwrapped, because a composition
        // can change under an open menu.
        Action::TrainPost { post_id, verb } => {
            let manager = app.feed.manager.clone()?;
            let factor = manager.train_target_factor()?;
            // Tapping the already-active verb un-marks it (the toggle semantics
            // ui.yaml's component spec states); tapping the other verb re-trains.
            let undo = manager.example_label_for(&post_id, &factor) == Some(verb);
            app.feed.actions_open = None;
            Some(Op::TrainPost {
                manager,
                post_id,
                factor,
                verb,
                undo,
            })
        }

        // --- local: the create-feed form ---
        Action::OpenCreateFeed => {
            app.feed.form = CreateFeedForm::fresh();
            app.feed.mode = Mode::CreateFeed;
            app.feed.actions_open = None;
            None
        }
        Action::CancelCreateFeed => {
            app.feed.mode = Mode::List;
            None
        }
        Action::SetRuleType(t) => {
            app.feed.form.rule_type = t;
            None
        }
        Action::ToggleRuleRequired => {
            app.feed.form.rule_required = !app.feed.form.rule_required;
            None
        }
        Action::AddRule => {
            let form = &mut app.feed.form;
            // The label rules pack both inputs into one wire value as
            // `"category:threshold"`; every other type sends the value alone
            // (the shared encoder splits on ':' — linux's `format!` shape).
            let kind = rule_type_options()
                .iter()
                .find(|o| o.value == form.rule_type)
                .map(|o| o.input_kind)
                .unwrap_or(fauna_client_feed::RuleInputKind::Text);
            let value = if kind == fauna_client_feed::RuleInputKind::TextAndNumber {
                format!("{}:{}", form.rule_value, form.rule_threshold)
            } else {
                form.rule_value.clone()
            };
            form.rules.push(FilterRuleInput {
                rule_type: form.rule_type.clone(),
                value,
                required: form.rule_required,
            });
            form.rule_value.clear();
            form.rule_threshold = fauna_client_feed::DEFAULT_RULE_THRESHOLD.to_string();
            form.rule_required = false;
            None
        }
        Action::SetCombination(c) => {
            app.feed.form.combination = c;
            None
        }
        Action::SetFactor(f) => {
            app.feed.form.factor = f;
            None
        }
        Action::ToggleFactorGlobal => {
            app.feed.form.factor_global = !app.feed.form.factor_global;
            None
        }
        Action::AddFactor => {
            let form = &mut app.feed.form;
            form.factors.push(FactorWeightInput {
                factor: form.factor.clone(),
                // The editor takes a decimal multiplier ("2.0"); the wire takes
                // signed permille. The conversion is shared, so every app
                // rounds identically.
                weight_permille: fauna_core::format::parse_weight_permille(&form.factor_weight),
                global: form.factor_global,
            });
            form.factor_weight.clear();
            form.factor_global = false;
            None
        }

        // --- local: the bridge-subscribe form ---
        Action::OpenBridgeForm => {
            // Pre-select the first available bridge — `CreateFeedForm::fresh`'s
            // rule-type precedent, so the select never opens on a blank value.
            let kind = manager
                .snapshot()
                .available_bridges
                .first()
                .map(|b| b.id.clone())
                .unwrap_or_default();
            app.feed.bridge_form = BridgeSubscribeForm {
                kind,
                ..Default::default()
            };
            app.feed.bridge_form_open = true;
            app.feed.actions_open = None;
            None
        }
        Action::CancelBridgeForm => {
            app.feed.bridge_form_open = false;
            None
        }
        Action::SetBridgeKind(kind) => {
            app.feed.bridge_form.kind = kind;
            None
        }
        Action::SubscribeBridge => {
            let form = app.feed.bridge_form.clone();
            // Empty-name-defaults-to-URI — linux's client-glue fallback
            // (`feed_list.rs:799`); the shared manager takes `name` as a plain
            // arg with no default of its own.
            let name = if form.name.is_empty() {
                form.uri.clone()
            } else {
                form.name
            };
            // Close optimistically, the `Action::CreateFeed` precedent: a
            // failure surfaces on `error-message` over the list, ui.yaml
            // defines no form-scoped error slot here.
            app.feed.bridge_form_open = false;
            app.feed.bridge_form = BridgeSubscribeForm::default();
            Some(Op::SubscribeBridge {
                manager,
                kind: form.kind,
                uri: form.uri,
                name,
            })
        }
        Action::UnsubscribeBridge(id) => Some(Op::UnsubscribeBridge { manager, id }),
    }
}

/// The network half of a feed gesture — owns only `Arc`s, so it can be awaited
/// on the agent's path or spawned on the keyboard's. Resolves to an [`Outcome`]:
/// an error to surface on `error-message` / `compose-error`, or `Done` on
/// success.
/// What the ⋯ menu's last copy affordance produced: the exact string it put on
/// the clipboard, the post it belongs to, and which of the two verbs fired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopiedFeedLink {
    pub post_id: String,
    pub kind: CopiedKind,
    pub url: String,
}

/// Which copy affordance produced a [`CopiedFeedLink`]. The two differ in what
/// they hand out — a durable public URL vs. a short-TTL capability URL — so the
/// confirmation line they paint differs too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopiedKind {
    /// `feed-post-copy-web-link-button` — the tokenless public page URL.
    Web,
    /// `feed-post-copy-paywall-link-button` — a freshly minted short-TTL
    /// full-access URL.
    Paywall,
}

pub enum Op {
    SelectFeed {
        manager: Arc<CliFeedManager>,
        id: String,
    },
    /// `fauna.web.publish.set` on an own post, then a re-read of the feed so the
    /// card's `web_slug` — and with it the whole verb family — repaints from the
    /// nest's own answer rather than optimistically.
    WebPublish {
        nest: Arc<NestClient>,
        manager: Arc<CliFeedManager>,
        post_id: String,
    },
    /// `fauna.web.publish.unset`, then the same re-read. Idempotent nest-side,
    /// which is what lets the verb be one tap with no confirm step.
    WebUnpublish {
        nest: Arc<NestClient>,
        manager: Arc<CliFeedManager>,
        post_id: String,
    },
    /// `fauna.web.paywall.mint_token` for a published+gated own post, carrying
    /// the resolved `origin` and `slug` so the fold builds the tokened URL
    /// without re-resolving them.
    WebMintPaywallLink {
        nest: Arc<NestClient>,
        post_id: String,
        slug: String,
        origin: String,
    },
    /// Read the origin inputs the ⋯ menu's copy affordances resolve against,
    /// for a user who reached them without ever opening Settings → Web.
    ///
    /// **Why this exists at all:** those inputs live in `WebSettingsState`, which
    /// only that page's nav edge hydrates. Painting a *disabled* copy verb off
    /// unread state would tell a creator with a perfectly good address that they
    /// have none — the same lie the settings page's own hydrate gate exists to
    /// prevent. So the menu reads them once, through the settings page's own
    /// `read_web_page`, rather than growing a second answer.
    HydrateWebOrigin {
        nest: Arc<NestClient>,
        handle: String,
    },
    /// The built-in **Trending** virtual feed (`trending.md` § The Trending
    /// feed) — the scored sibling of the local feed over
    /// `fauna.feed.trending.posts`, driven through the shared
    /// `FeedManager::select_trending_feed`. No id: there is no feed row.
    SelectTrending {
        manager: Arc<CliFeedManager>,
    },
    ClearSearch {
        manager: Arc<CliFeedManager>,
    },
    /// Re-run the current query on entering the Feed tab
    /// ([`crate::feed::nav_enter_op`]) — the shared
    /// `FeedManager::refresh_current_feed`.
    RefreshCurrentFeed {
        manager: Arc<CliFeedManager>,
    },
    LoadMore {
        manager: Arc<CliFeedManager>,
    },
    SubmitPost {
        manager: Arc<CliFeedManager>,
        content: Option<Arc<dyn NestContentApi>>,
        staged: Option<String>,
    },
    Interact {
        manager: Arc<CliFeedManager>,
        post_id: String,
        action: String,
    },
    /// `feed-reply-submit-button` — through the shared `FeedManager::reply`,
    /// never the raw `interact` door (`ui/feed.md` § Implementation status
    /// today: the native arm discards `body` outright, so a reply typed here
    /// would be acked as success and dropped on the floor).
    Reply {
        manager: Arc<CliFeedManager>,
        /// The bulk plane a **sealed** reply's body is uploaded on — the
        /// composer's own handle ([`Op::SubmitPost`]); unused by a public reply.
        content: Option<Arc<dyn NestContentApi>>,
        post_id: String,
        body: String,
        /// `feed-reply-public-confirm` was checked: send through
        /// `FeedManager::reply_public_confirmed`, the only door that composes
        /// words public under a restricted target (ruling 5's confirmation).
        public_confirmed: bool,
    },
    /// Make `post_id` renderable on the `post_detail` sub-page, in the two
    /// ordered steps a detail open can need:
    ///
    /// 1. **Resolve** — `FeedManager::resolve_post`, a no-op when the timeline
    ///    already holds the post (every `post-card` click) and a single
    ///    `fauna.posts.get` for a **deep link** the feed never loaded (a search
    ///    hit; `ui/search.md` § Where logic lives → *Result navigation*).
    /// 2. **Unseal** — a gated post not already unsealed this session needs its
    ///    sealed full-body blob fetched and decrypted (`ui/feed.md` § Encryption
    ///    at rest). Reaching the bytes is platform glue (the bulk plane, like
    ///    every other blob fetch); the decrypt itself is shared — custody period
    ///    key for the author, KeyBlob entry for an entitled reader.
    ///
    /// Step 2 has to follow step 1 rather than ride a second action, because a
    /// post outside the timeline has no snapshot row to ask "is this gated?" of
    /// until the fetch lands. `content` is `None` on a seat with no content API;
    /// step 1 still runs (the unseal is what needs the blob plane).
    OpenPostDetail {
        manager: Arc<CliFeedManager>,
        content: Option<Arc<dyn NestContentApi>>,
        post_id: String,
    },
    /// `gated-post-buy-button` — the self-serve teaser purchase (gap (2c),
    /// `monetization.md` § Per-post pay-to-unlock): subscribe against the
    /// resolved offer's tier via the shared manager, no claim code needed.
    #[cfg(feature = "payments")]
    BuyUnlockOffer {
        manager: Arc<CliFeedManager>,
        post_id: String,
    },
    CreateFeed {
        manager: Arc<CliFeedManager>,
        form: CreateFeedForm,
    },
    /// `feed-delete-button`.
    DeleteFeed {
        manager: Arc<CliFeedManager>,
        feed_id: String,
    },
    /// `feed-post-delete-confirm-button` — destroy an own post. The manager
    /// owns the whole call (it builds the signed `Tombstone` and drives the
    /// shared `PostsClient::posts_delete`), so this carries nothing but the id
    /// — the same shape linux's `m.delete_post(pid)` takes.
    DeletePost {
        manager: Arc<CliFeedManager>,
        post_id: String,
    },
    /// `bridge-form-subscribe-button`.
    SubscribeBridge {
        manager: Arc<CliFeedManager>,
        kind: String,
        uri: String,
        name: String,
    },
    /// `bridge-feed-unsubscribe-button`.
    UnsubscribeBridge {
        manager: Arc<CliFeedManager>,
        id: i64,
    },
    /// A training gesture on the open ⋯ overflow. `undo` un-marks instead of
    /// training — one op for both, because the shared manager re-seals, re-ranks
    /// and notifies either way, so there is a single fold.
    TrainPost {
        manager: Arc<CliFeedManager>,
        post_id: String,
        factor: String,
        verb: fauna_feed::TrainVerb,
        undo: bool,
    },
    /// Fetch + rasterize the `post-image` blobs for `hashes`. GETs each blob by
    /// hash off the render thread (`fire_resolves` already folded the hash into
    /// the document; this resolves the bytes), hands what came back to
    /// [`FeedManager::open_media_bytes`](fauna_feed::FeedManager::open_media_bytes),
    /// and folds the art into the page's [`ImageCache`].
    ///
    /// The manager rides along for that one call, and it is not a mutation: a
    /// **public** post's blob is plaintext on the wire and comes straight back,
    /// while a **gated** post's attachment is AEAD-sealed under the per-post key
    /// its body opened under and has to be opened before rasterizing
    /// (`docs/goal/ui/media.md` § Encryption at rest). Routing every hash
    /// through the one seam is what keeps this op — and its six siblings on the
    /// other apps — free of any is-this-post-gated branch.
    ///
    /// `paths` are a bridged post's nest-relative proxied pictures
    /// ([`RenderBlock::ProxiedImage`](fauna_core::render::RenderBlock::ProxiedImage),
    /// render-model.md § D6c): the same bearer-carrying `content.get`, with the
    /// path as its argument instead of `paths::blob::by_hash`. Their bytes are the
    /// nest proxy's plaintext answer — never a sealed attachment — so they skip
    /// the open seam; their art is cached under the path.
    FetchImage {
        content: Arc<dyn NestContentApi>,
        manager: Arc<CliFeedManager>,
        hashes: Vec<String>,
        paths: Vec<String>,
    },
    /// Fetch + rasterize the **revealed** `doc-remote-image` bodies for `urls`.
    /// The sibling of [`Op::FetchImage`] with the one difference that matters:
    /// these urls are third-party hosts, not this nest, so they go over
    /// [`crate::remote_image`]'s bare client instead of the bulk plane — which is
    /// also why no `content` rides along. Only ever built from
    /// [`crate::document::revealed_remote_image_urls`], so a blocked image's url
    /// can never reach it.
    FetchRemoteImages {
        urls: Vec<String>,
    },
    /// Resolve C2PA provenance for the `post-image` blobs at `hashes`, folded
    /// into [`FeedState::c2pa`]. Independent of [`Op::FetchImage`]: a badge and
    /// its art are two different reads of the same blob and resolve on their
    /// own schedules.
    ///
    /// **Two stages, and the second one is the verdict** (`ui/media.md` §
    /// C2PA provenance — the badge-correction rule):
    ///
    /// 1. `HEAD /api/v1/blob/<hash>` reads the `x-c2pa` response header. That
    ///    header is the *uploader's own assertion* — the nest stores
    ///    `UploadSidecar.has_c2pa` for a public-post blob without ever
    ///    inspecting the bytes — so it decides only whether to bother with
    ///    stage 2. `false` ends the check (nothing to verify, no bytes
    ///    fetched); it is the answer for essentially every post.
    /// 2. For the few that claim provenance, GET the blob, open it through
    ///    [`FeedManager::open_media_bytes`](fauna_feed::FeedManager::open_media_bytes)
    ///    — the same one seam [`Op::FetchImage`] uses, so a gated post's
    ///    sealed attachment is unsealed and a public one passes through — and
    ///    hand the plaintext to `fauna_media::process::detect_c2pa_in_bytes`.
    ///    **That call is the badge**: a modified client can put `has_c2pa =
    ///    true` on an image with no manifest, and painting from stage 1 alone
    ///    would show the fauna provenance badge on the strength of its say-so.
    ///
    /// The extra GET is bounded by the posts that claim provenance, which is
    /// why the header survives as a pre-filter rather than being dropped.
    FetchC2pa {
        content: Arc<dyn NestContentApi>,
        manager: Arc<CliFeedManager>,
        hashes: Vec<String>,
    },
}

/// What an [`Op`] resolved to; folded back into the page by [`apply_outcome`].
#[derive(Debug)]
pub enum Outcome {
    /// The op completed. The manager's own snapshot tick drives the redraw,
    /// so there is nothing to fold back here.
    Done,
    /// A transport/validation failure — lands on `error-message` /
    /// `compose-error` via [`apply_outcome`].
    Error(String),
    /// Rasterized `post-image` art per hash — or [`Finished::Failed`] for a
    /// blob that is absent, will not open or will not decode, or
    /// [`Finished::Transient`] for a nest that could not serve it right now,
    /// which the fold forgets so the next kick retries
    /// (`fauna_core::load_cache`'s module doc). A pure cache write, folded into
    /// [`FeedState::images`]. Never touches the page banner: one unreadable
    /// image is a placeholder, not an error, exactly like a Media thumbnail
    /// (`ui/media.md` § Thumbnails).
    Images(Vec<(String, Finished<crate::thumbnail::Thumbnail>)>),
    /// The same pure cache write for `doc-remote-image`, keyed by **url** rather
    /// than content hash, into [`FeedState::remote_images`]. A separate cache
    /// and a separate variant because the two key spaces mean different things:
    /// a hash entry can never go stale, a url's can.
    RemoteImages(Vec<(String, Option<crate::thumbnail::Thumbnail>)>),
    /// `has_c2pa` per hash — a pure cache write into [`FeedState::c2pa`]. A
    /// refused request degrades to `false` (no badge), the same posture as an
    /// unreadable image: a provenance check is a UI hint, never a page error.
    /// A transient one is [`Finished::Transient`], forgotten like an image's.
    C2pa(Vec<(String, Finished<bool>)>),
    /// The web-origin inputs the ⋯ menu's copy affordances resolve against,
    /// folded into the settings page's own state so both surfaces read one
    /// answer. A read failure lands on `error-message` like any other.
    WebOrigin(crate::settings::WebPageRead),
    /// A freshly minted paywall link, already on the clipboard — folded into
    /// [`FeedState::web_copied`] so the button repaints its `copied` attr and
    /// the menu shows what was handed out. Carries the same value that reached
    /// `copy_to_clipboard`, never a re-derivation, so the two cannot drift.
    CopiedPaywallLink(CopiedFeedLink),
}

impl Op {
    pub async fn run(self) -> Outcome {
        match self {
            Op::SelectFeed { manager, id } => {
                manager.select_feed(Some(id)).await;
                Outcome::Done
            }
            Op::SelectTrending { manager } => {
                manager.select_trending_feed().await;
                Outcome::Done
            }
            Op::RefreshCurrentFeed { manager } => {
                // Re-entering the Feed tab must re-pull the bridge-availability
                // signal too — linking/unlinking a bridge on a different page
                // and walking back must not keep showing a stale
                // `bridge-feed-subscribe-toggle`/subscription list, the same
                // staleness class this op already exists to close for the
                // sealed scorers (linux's `connect_map`, `views/feed/mod.rs:201-204`).
                //
                // And the page-load read itself — linux's hook fires it first.
                // `refresh_feeds` is what carries the composer's audience
                // options (`own_tiers`, `own_rooms`), so without it a tier the
                // author had just minted on the Tiers tab was never offered in
                // `compose-gate-tier-select` until the app restarted: the
                // manager's own doc on `refresh_feeds` says every app calls it
                // on entering the Feed page, and tui called it only at session
                // start.
                manager.refresh_feeds().await;
                manager.refresh_bridge_feeds().await;
                manager.refresh_available_bridges().await;
                manager.refresh_current_feed().await;
                Outcome::Done
            }
            Op::ClearSearch { manager } => {
                manager.clear_search().await;
                Outcome::Done
            }
            // ── the own-post web-publishing verbs ──
            //
            // Both mutations end in `refresh_current_feed`, never a local edit:
            // the card's `web_slug` is what drives the whole verb family, so
            // repainting it from the nest's own answer is what keeps a
            // half-applied publish showing as unpublished rather than as a post
            // the user believes is live.
            Op::WebPublish {
                nest,
                manager,
                post_id,
            } => match decode_post_id(&post_id) {
                Err(e) => Outcome::Error(e),
                Ok(bytes) => {
                    // `None` slug: the default is the nest's to mint, and a
                    // client-chosen one is not a surface this menu offers.
                    match fauna_client_web::WebClient::new(nest)
                        .publish_set(bytes, None)
                        .await
                    {
                        Err(e) => Outcome::Error(fauna_i18n::strings::web_publish::error_publish(
                            &e.to_string(),
                        )),
                        Ok(_) => {
                            manager.refresh_current_feed().await;
                            Outcome::Done
                        }
                    }
                }
            },
            Op::WebUnpublish {
                nest,
                manager,
                post_id,
            } => match decode_post_id(&post_id) {
                Err(e) => Outcome::Error(e),
                Ok(bytes) => match fauna_client_web::WebClient::new(nest)
                    .publish_unset(bytes)
                    .await
                {
                    Err(e) => Outcome::Error(fauna_i18n::strings::web_publish::error_unpublish(
                        &e.to_string(),
                    )),
                    Ok(_) => {
                        manager.refresh_current_feed().await;
                        Outcome::Done
                    }
                },
            },
            Op::HydrateWebOrigin { nest, handle } => {
                let client = fauna_client_web::WebClient::new(nest);
                match crate::settings::read_web_page(&client, &handle).await {
                    Ok(read) => Outcome::WebOrigin(read),
                    Err(e) => Outcome::Error(e),
                }
            }
            Op::WebMintPaywallLink {
                nest,
                post_id,
                slug,
                origin,
            } => {
                match fauna_client_web::WebClient::new(nest)
                    .paywall_mint_token(fauna_client_web::PaywallTarget::PostSlug { slug })
                    .await
                {
                    Err(e) => Outcome::Error(fauna_i18n::strings::web_publish::error_paywall_link(
                        &e.to_string(),
                    )),
                    // The nest's own `path` — never a client-rebuilt
                    // `post/{slug}.html`, which would silently diverge the day
                    // the render layout moves.
                    Ok(minted) => {
                        let url =
                            fauna_client_web::tokened_url(&origin, &minted.path, &minted.token);
                        crate::wizard::copy_to_clipboard(&url);
                        Outcome::CopiedPaywallLink(CopiedFeedLink {
                            post_id,
                            kind: CopiedKind::Paywall,
                            url,
                        })
                    }
                }
            }
            Op::LoadMore { manager } => {
                manager.load_more().await;
                Outcome::Done
            }
            Op::Interact {
                manager,
                post_id,
                action,
            } => {
                // `like`/`unlike` are RECORDED against the target; `quote` is
                // COMPOSED — it creates a post referencing the target, which is
                // the only thing that moves `quote_count`. Routing quote through
                // `interact` looks identical and does nothing at all on a native
                // post: the nest's arm discards the call and creates no post
                // (`ui/feed.md` § Implementation status today).
                //
                // `repost` is the manager's TOGGLE (`feed.md` § Interaction bar
                // → Repost, ratified 2026-08-10): off → composes the caller's
                // empty-body `Reference::Repost` post; on → un-reposts it via
                // the viewer_repost_id the projection now carries. Bridged
                // sources route through interact inside the manager.
                //
                // `like` is the manager's other TOGGLE, off `viewer_liked`:
                // both directions ride the same interact door on the same
                // post id (a like is recorded, not composed), so the only
                // thing this arm buys over a bare `interact` is the OFF
                // direction — which was unreachable from every app's UI
                // because the nest's like arm is idempotent per (actor, post).
                let failed = match action.as_str() {
                    "quote" => manager.quote(post_id, String::new()).await.err(),
                    "repost" => manager.repost(post_id).await.err(),
                    "like" => manager.like(post_id).await.err(),
                    _ => manager.interact(post_id, action, None).await.err(),
                };
                match failed {
                    Some(e) => Outcome::Error(refusal_copy(e)),
                    None => Outcome::Done,
                }
            }
            Op::Reply {
                manager,
                content,
                post_id,
                body,
                public_confirmed,
            } => match send_reply(&manager, content, post_id, body, public_confirmed).await {
                None => Outcome::Done,
                Some(e) => Outcome::Error(refusal_copy(e)),
            },
            Op::OpenPostDetail {
                manager,
                content,
                post_id,
            } => {
                // Step 1 — make the post exist in the snapshot at all.
                if let fauna_feed::PostResolution::TakenDown { .. } =
                    manager.resolve_post(post_id.clone()).await
                {
                    // The nest withholds a legally-taken-down post's body from
                    // every viewer, so there is no body to paint — but the
                    // manager parks a tombstone `PostSummary` in the deep-link
                    // slot, and `post_detail_elements` renders the shared string
                    // from its `legal_takedown_ref` IN THE BODY AREA (the
                    // `quoted-post` embed's and the DM bubble's posture; `ui/feed.md`
                    // § The read model → *Opening a post the timeline never loaded*).
                    // So this arm only skips the unseal below — a withheld post
                    // is never a gated one — and deliberately raises NO page
                    // error: the notice is in place, not on `error-message`.
                    return Outcome::Done;
                }
                // Step 2 — only a gated post that has not already been unsealed
                // this session costs the blob round trip; `unlock_gated_post` is
                // idempotent but the fetch is not free, and the (now-resolved)
                // snapshot tells us which case this is
                // (`PostSummary::{gated_tier,gated_unlocked}`).
                let needs_unseal = manager
                    .snapshot()
                    .find_post(&post_id)
                    .is_some_and(|p| p.gated_tier.is_some() && !p.gated_unlocked);
                let Some(content) = content.filter(|_| needs_unseal) else {
                    return Outcome::Done;
                };
                // A failure here leaves the post TEASED, never errored: the
                // reader may simply not be entitled, which is the product's
                // normal state for someone else's paywalled post — surfacing it
                // on `error-message` would turn "you haven't bought this" into
                // a page error (linux's discipline, `client.rs:1708`).
                let Some(hash) = manager.gated_blob_hash(post_id.clone()).await else {
                    return Outcome::Done;
                };
                if let Ok(bytes) = content.get(&paths::blob::by_hash(&hash)).await {
                    let _ = manager.unlock_gated_post(post_id, bytes.to_vec()).await;
                }
                Outcome::Done
            }
            #[cfg(feature = "payments")]
            Op::BuyUnlockOffer { manager, post_id } => {
                match manager.buy_unlock_offer(post_id).await {
                    // `None`: the offer isn't resolved (shouldn't happen — the
                    // button's render is gated on it), so there is nothing to do
                    // rather than a confusing error.
                    None | Some(Ok(_)) => Outcome::Done,
                    Some(Err(e)) => Outcome::Error(feed::error_buy_unlock(&e)),
                }
            }
            Op::CreateFeed { manager, form } => match manager
                .create_feed(
                    form.name,
                    form.rules,
                    form.combination,
                    None,
                    None,
                    form.factors,
                )
                .await
                .err()
            {
                Some(e) => Outcome::Error(e),
                None => Outcome::Done,
            },
            Op::DeleteFeed { manager, feed_id } => match manager.delete_feed(feed_id).await.err() {
                Some(e) => Outcome::Error(e),
                None => Outcome::Done,
            },
            // The manager removes the row from the loaded window on success, so
            // the card disappears off the same snapshot tick — no re-read, and
            // the failure text is the shared `feed.error_delete` linux paints.
            Op::DeletePost { manager, post_id } => match manager.delete_post(post_id).await.err() {
                Some(e) => Outcome::Error(feed::error_delete(&e)),
                None => Outcome::Done,
            },
            Op::SubscribeBridge {
                manager,
                kind,
                uri,
                name,
            } => match manager.subscribe_bridge(kind, uri, name).await.err() {
                Some(e) => Outcome::Error(e),
                None => Outcome::Done,
            },
            Op::UnsubscribeBridge { manager, id } => {
                match manager.unsubscribe_bridge(id).await.err() {
                    Some(e) => Outcome::Error(e),
                    None => Outcome::Done,
                }
            }
            Op::TrainPost {
                manager,
                post_id,
                factor,
                verb,
                undo,
            } => {
                // The manager re-seals, puts, re-ranks the loaded window and
                // notifies on its own — the shell owes nothing but the error
                // bridge. Without that bridge a refused train would be a silent
                // no-op (testing.md point 11).
                let failed = if undo {
                    manager.untrain_post(post_id, factor).await.err()
                } else {
                    manager.train_post(post_id, factor, verb).await.err()
                };
                match failed {
                    Some(e) => Outcome::Error(e),
                    None => Outcome::Done,
                }
            }
            Op::SubmitPost {
                manager,
                content,
                staged,
            } => match submit_post(manager, content, staged).await {
                Some(e) => Outcome::Error(e),
                None => Outcome::Done,
            },
            Op::FetchImage {
                content,
                manager,
                hashes,
                paths,
            } => {
                // Concurrently, like Media: N independent by-hash GETs that would
                // otherwise each wait for the one before it (`GET /api/v1/blob/<hash>`,
                // the bulk-binary carve-out). The one shared call between fetch and
                // decode opens a gated post's sealed attachment and hands a public
                // post's plaintext straight back, so this arm never asks which it has.
                let arts = futures_util::future::join_all(hashes.into_iter().map(|hash| {
                    let content = Arc::clone(&content);
                    let manager = Arc::clone(&manager);
                    async move {
                        // A miss, a body that will not open, or an undecodable
                        // one is `Failed`: the placeholder stays, never a banner
                        // (shared degrade). A nest that could not serve it right
                        // now is `Transient` — forgotten, so the next kick
                        // retries rather than blanking the image for the session.
                        let art = match content.get(&paths::blob::by_hash(&hash)).await {
                            Ok(bytes) => manager
                                .open_media_bytes(&hash, bytes.to_vec())
                                .and_then(|plain| {
                                    crate::thumbnail::rasterize(
                                        &plain,
                                        crate::thumbnail::POST_IMAGE_COLS,
                                    )
                                })
                                .into(),
                            Err(e) if e.is_transient() => Finished::Transient,
                            Err(_) => Finished::Failed,
                        };
                        (hash, art)
                    }
                }));
                // A bridged post's proxied picture: fetched from this nest with the
                // bearer like a blob, rasterized the same way, cached by its path.
                let proxied = futures_util::future::join_all(paths.into_iter().map(|path| {
                    let content = Arc::clone(&content);
                    async move {
                        let art = match content.get(&path).await {
                            Ok(bytes) => crate::thumbnail::rasterize(
                                &bytes,
                                crate::thumbnail::POST_IMAGE_COLS,
                            )
                            .into(),
                            Err(e) if e.is_transient() => Finished::Transient,
                            Err(_) => Finished::Failed,
                        };
                        (path, art)
                    }
                }));
                let (mut arts, proxied) = futures_util::future::join(arts, proxied).await;
                arts.extend(proxied);
                Outcome::Images(arts)
            }
            Op::FetchRemoteImages { urls } => Outcome::RemoteImages(
                crate::remote_image::fetch_all(urls, crate::thumbnail::POST_IMAGE_COLS).await,
            ),
            Op::FetchC2pa {
                content,
                manager,
                hashes,
            } => {
                let results = futures_util::future::join_all(hashes.into_iter().map(|hash| {
                    let content = Arc::clone(&content);
                    let manager = Arc::clone(&manager);
                    async move {
                        // Stage 1 — the uploader's assertion, used only to
                        // decide whether the bytes are worth fetching. A refusal
                        // degrades to `false` (no badge); a nest that could not
                        // answer right now leaves the verdict unsettled, so the
                        // next kick asks again (either stage).
                        let asserted =
                            match content.head_has_c2pa(&paths::blob::by_hash(&hash)).await {
                                Ok(asserted) => asserted,
                                Err(e) if e.is_transient() => return (hash, Finished::Transient),
                                Err(_) => false,
                            };
                        if !asserted {
                            return (hash, Finished::Loaded(false));
                        }
                        // Stage 2 — ground truth over the bytes the nest
                        // actually serves. A miss or a body that will not open
                        // is a `false` for the same reason `Op::FetchImage`
                        // degrades to a placeholder: an unverifiable claim is
                        // not a verified one.
                        let verified = match content.get(&paths::blob::by_hash(&hash)).await {
                            Ok(bytes) => {
                                manager.open_media_bytes(&hash, bytes.to_vec()).is_some_and(
                                    |plain| fauna_media::process::detect_c2pa_in_bytes(&plain),
                                )
                            }
                            Err(e) if e.is_transient() => return (hash, Finished::Transient),
                            Err(_) => false,
                        };
                        (hash, Finished::Loaded(verified))
                    }
                }))
                .await;
                Outcome::C2pa(results)
            }
        }
    }
}

/// A manager error as `error-message` paints it: a **stated refusal** (a reply
/// or quote with text under a restricted post — `ui/feed.md` § Encryption at
/// rest → *A reply, quote or repost of a restricted post*) reads in the
/// user's language; every other error keeps its own text.
fn refusal_copy(e: String) -> String {
    fauna_feed::refusal_i18n_key(&e)
        .and_then(fauna_i18n::strings::lookup)
        .map(str::to_string)
        .unwrap_or(e)
}

/// Fold an [`Outcome`] back into the page: an error lands on the page's
/// canonical `error-message` (`compose-error` reads the same `Page::Feed`
/// slot); a success clears any prior one — the manager's own snapshot tick
/// (`FeedSnapshotObserver::on_changed`) still drives the redraw, this is only
/// the error-slot half. Unified with the six other `Outcome`-shaped pages,
/// each of which clears on its own success variant (`settings::Outcome::
/// FilterCreated`, `events::Outcome::EventMutated`, …) — every feed mutation
/// collapses into this one `Done` variant, so success on ANY gesture clears a
/// stale error from an unrelated prior failure, matching `app.errors` being
/// page-scoped rather than gesture-scoped everywhere else.
pub fn apply_outcome(app: &mut App, outcome: Outcome) {
    match outcome {
        Outcome::Done => {
            app.errors.remove(&crate::pages::Page::Feed);
        }
        Outcome::Error(e) => {
            app.errors.insert(crate::pages::Page::Feed, e);
        }
        // A pure cache write — the art arrived rasterized (the op did that work
        // off the render thread). No error-slot touch in either direction: a
        // `post-image` never reaches the banner, so folding one must not clear an
        // unrelated prior compose error either (mirrors Media's `Thumbnails`).
        Outcome::Images(arts) => {
            for (hash, art) in arts {
                app.feed.images.finish(hash, art);
            }
        }
        // Same pure cache write, other key space (url, not content hash).
        Outcome::RemoteImages(arts) => {
            for (url, art) in arts {
                app.feed.remote_images.set(url, art);
            }
        }
        // Same posture as `Images`: a provenance check is a UI hint, never a
        // page error, so a refused check folds in as `false` (already done by
        // `Op::run`) rather than touching the error slot, and a transient one
        // is forgotten.
        Outcome::C2pa(results) => {
            for (hash, has_c2pa) in results {
                app.feed.c2pa.finish(hash, has_c2pa);
            }
        }
        // The mint succeeded and the link is already on the clipboard; folding
        // it is what repaints the button's `copied` attr and the confirmation
        // line. Clears the error slot for the same reason `Done` does — this IS
        // the success path, just one that carries a value back.
        Outcome::CopiedPaywallLink(copied) => {
            app.errors.remove(&crate::pages::Page::Feed);
            app.feed.web_copied = Some(copied);
        }
        // Folded into the SETTINGS page's state, not the feed's: it is the same
        // read that page performs, and duplicating it here is how the two
        // surfaces would start disagreeing about a creator's address.
        Outcome::WebOrigin(read) => {
            app.errors.remove(&crate::pages::Page::Feed);
            app.settings.apply_web_page_read(read);
        }
    }
}

/// Upload the staged blob (if any), stage the resolved [`AttachedFile`], and let
/// the shared `submit_post` validate + build + sign + create + reload.
///
/// `submit_post` requires the `blob_hash` **already resolved** (`manager.rs`:
/// "the blob upload is platform glue"), which is why the upload happens here and
/// not inside the manager. Both halves of the attach flow — a human typing a
/// path into `compose-file`, and the driver's `compose.file` state patch — stage
/// the same value, so they converge here.
///
/// **Four compose flows converge on one submit** (`ui/feed.md` § Encryption at
/// rest; `monetization.md` § Pillars 2+3 and § Per-post pay-to-unlock):
///
/// - **Public** — `submit_post`, plaintext body.
/// - **Gated to a tier** — `prepare_gated_blob` seals the full body under the
///   tier's period key.
/// - **Addressed to a room** — `prepare_gated_blob` again: its room arm seals
///   under the room's key through the installed seam, so this glue adds
///   nothing for it but the upload sidecar the staged post names.
/// - **Sell this post…** — `prepare_sell_post` auto-mints a degenerate
///   single-post tier *and* seals, in one forced-ordering call.
///
/// The last two are identical from here on: each returns a sealed blob to
/// upload on the bulk plane, then finishes through the **unchanged**
/// `submit_gated_post` / `abort_gated_submit` pair, because `prepare_sell_post`
/// stages into the same `pending_gated` slot. That is why adding "sell" needed
/// no new upload glue — the whole point of the shared shape.
async fn submit_post(
    manager: Arc<CliFeedManager>,
    content: Option<Arc<dyn NestContentApi>>,
    staged: Option<String>,
) -> Option<String> {
    let sell = manager.snapshot().compose.sell;

    // A SOLD post's attachment seals under the tier the sale itself mints — and
    // that tier does not exist yet, so the mint is split in two and its first
    // half has to run before `stage_attachment` can reach a period key. Only
    // when there is actually an attachment: with none, `prepare_sell_post` runs
    // this itself and the flow is the single call it has always been.
    if let (Some(sell), Some(_)) = (sell.as_ref(), staged.as_ref())
        && let Err(e) = manager
            .stage_sell_tier(
                sell.subscribers_get_it_free,
                sell.asking_price.trim().parse::<u64>().ok(),
            )
            .await
    {
        // Already stamped on `compose-error` by the manager; nothing was
        // minted, because every validation runs ahead of the mint.
        return Some(e);
    }

    if let Some(path) = staged {
        // Borrowed, not moved: the gated flows below need `content` too.
        let content = content.as_ref()?;
        if let Some(e) = stage_attachment(&manager, content.as_ref(), &path).await {
            // Never submit a post whose attachment silently vanished: the user
            // asked for an image, so a failed upload fails the post.
            return Some(e);
        }
    }

    if let Some(sell) = sell {
        let content = content?;
        // Validation lives inside `stage_sell_tier` — which `prepare_sell_post`
        // runs itself when the block above did not — and stamps
        // `compose-error` there: a tier minted for a compose that never becomes
        // a post would be pure litter in the author's tier list, so it
        // validates before minting anything.
        let sealed = manager
            .prepare_sell_post(
                (!sell.price.trim().is_empty()).then(|| sell.price.clone()),
                sell.subscribers_get_it_free,
                // Empty or unparseable means no machine price — the tier stays
                // a tip target forever (`monetization.md` § The asking price).
                // `prepare_sell_post` owns the sats→msat conversion and the
                // overflow refusal; this is a plain text→u64 parse, nothing
                // else.
                sell.asking_price.trim().parse::<u64>().ok(),
            )
            .await
            .ok()?;
        return finish_gated(&manager, content.as_ref(), sealed).await;
    }

    match manager.prepare_gated_blob().await {
        Ok(Some(sealed)) => finish_gated(&manager, content?.as_ref(), sealed).await,
        // Not gated — an ordinary plaintext post.
        Ok(None) => manager.submit_post().await.err(),
        // Validation failure, already stamped on `compose-error`.
        Err(e) => Some(e),
    }
}

/// Send a reply: **sealed to the target's own audience** when the manager says
/// this device can author under it, the ordinary `reply` otherwise
/// (`ui/feed.md` § Encryption at rest → *Ruling 5's build — the shape*, (c)) —
/// or, with the dialog's explicit-public answer checked, the confirmed-public
/// verb ((e)), which is the one door that composes words public under a
/// restricted target and refuses where the reply could have sealed instead.
///
/// The composer's fall-through, reused: `prepare_sealed_reply` answers the
/// sealed body or *nothing to upload*; bytes go through the same
/// [`finish_gated`] the composer's gated post takes, and nothing falls through
/// to `reply` — which composes public, or refuses words under a restricted post
/// this reader cannot author under. This glue never decides which.
///
/// `Some(err)` on failure, `None` on success — the caller's error convention.
async fn send_reply(
    manager: &Arc<CliFeedManager>,
    content: Option<Arc<dyn NestContentApi>>,
    post_id: String,
    body: String,
    public_confirmed: bool,
) -> Option<String> {
    if public_confirmed {
        return manager.reply_public_confirmed(post_id, body).await.err();
    }
    match manager
        .prepare_sealed_reply(post_id.clone(), body.clone())
        .await
    {
        Ok(Some(sealed)) => match content {
            Some(content) => finish_gated(manager, content.as_ref(), sealed).await,
            None => {
                let e = "no upload plane for a sealed reply".to_string();
                manager.abort_gated_submit(e.clone());
                Some(e)
            }
        },
        Ok(None) => manager.reply(post_id, body).await.err(),
        Err(e) => Some(e),
    }
}

/// The file name a staged attachment carries — shared by the attach-time
/// handle and the submit's upload, so the refusal and the post name one file.
fn attachment_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into())
}

/// The hash-less handle for a typed `compose-file` path that names a regular
/// file: its `{name, size}` from a `stat` — no read, so a keystroke in the path
/// field stays cheap. `media_type` stays `None`: the seal's own answer sets it
/// at submit, beside the hash ([`stage_attachment`]).
fn picked_handle(path: &str) -> Option<AttachedFile> {
    let meta = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    Some(AttachedFile {
        name: attachment_name(path),
        size: meta.len(),
        blob_hash: None,
        media_type: None,
    })
}

/// Seal the picked file **for the composer's current audience**, upload it, and
/// stage the resolved [`AttachedFile`].
///
/// **The seal must be resolved before the bytes are POSTed.** Until 2026-09-07
/// this uploaded a `PublicPost` (plaintext) blob unconditionally and only then
/// looked at the gate, so attaching a photo and *then* picking a tier left a
/// readable copy of a restricted post's picture on the nest under a hash anyone
/// can fetch — blob GET is unauthenticated by design and the nest exposes no
/// blob DELETE, so the only fix is to never upload one. `seal_compose_attachment`
/// reads the composer's audience and seals accordingly; this glue is pure
/// transport (`ui/media.md` § Encryption at rest — the shared-Rust seal-by-id
/// helper: a tier's period key never leaves shared Rust).
///
/// `Some(err)` on failure, `None` on success — the caller's error convention.
async fn stage_attachment(
    manager: &Arc<CliFeedManager>,
    content: &dyn NestContentApi,
    path: &str,
) -> Option<String> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) => return Some(format!("read {path}: {e}")),
    };
    let size = raw.len() as u64;
    let name = attachment_name(path);

    let sealed = match manager.seal_compose_attachment(raw).await {
        Ok(sealed) => sealed,
        Err(e) => return Some(e),
    };
    let media_type = sealed.media_type.clone();
    let hash = match fauna_client::upload_prepared_blob(
        content,
        MultipartBlob {
            sidecar_cbor: sealed.primary.sidecar_cbor,
            bytes: sealed.primary.bytes,
        },
        sealed.thumbnail.map(|t| MultipartBlob {
            sidecar_cbor: t.sidecar_cbor,
            bytes: t.bytes,
        }),
    )
    .await
    {
        Ok(hash) => hash,
        Err(e) => return Some(e),
    };

    let snap = manager.snapshot();
    manager.update_compose(
        snap.compose.text,
        snap.compose.tags,
        Some(AttachedFile {
            name,
            size,
            blob_hash: Some(hash),
            // The sealed class's sidecar says `application/octet-stream`; the
            // real MIME rides inside the seal, so the `MediaItem` must take it
            // from the seal's own answer.
            media_type: Some(media_type),
        }),
    );
    None
}

/// Upload a sealed full-body blob on the bulk plane and create the post once the
/// nest echoes its content address.
///
/// Shared by the gate-to-tier and sell-this-post flows — see [`submit_post`].
/// A failed upload **aborts the staged submit** rather than leaving it pending:
/// `abort_gated_submit` clears `pending_gated` and surfaces the reason on
/// `compose-error`, so a retry re-stages cleanly instead of racing a
/// half-finished one.
async fn finish_gated(
    manager: &Arc<CliFeedManager>,
    content: &dyn NestContentApi,
    sealed: Vec<u8>,
) -> Option<String> {
    // The staged post names its own sidecar class — a room post's
    // `GroupRestrictedPost`, a tier's or a sale's `PeriodRestrictedPost` — so
    // this glue never decides one (`FeedManager::gated_upload_sidecar`).
    let sidecar = manager.gated_upload_sidecar();
    match fauna_client::upload_sealed_post_blob(content, sidecar, sealed).await {
        Ok(hash) => manager.submit_gated_post(hash).await.err(),
        Err(e) => {
            manager.abort_gated_submit(e.clone());
            Some(e)
        }
    }
}

// ── Resolve triggers ────────────────────────────────────────────────────────

/// Fire the lazy embed resolves this snapshot still needs — media, quoted posts,
/// link previews.
///
/// **Guarded on document *shape*, never a per-tick flag** (linux's discipline):
/// each resolve folds a block into the document, and the folded block is itself
/// what closes the guard. A boolean "already asked" flag would go stale the
/// moment the manager re-emitted a snapshot, and a per-tick trigger would loop
/// forever — the manager notifies on every mutation, including the ones these
/// resolves cause.
pub fn fire_resolves(state: &FeedState) {
    let (Some(manager), Some(snapshot)) = (state.manager.clone(), state.snapshot()) else {
        return;
    };
    // Walks the RENDERED set, not just the timeline: a deep-linked post (a
    // search hit the feed never loaded, parked in the snapshot's deep-link slot)
    // owes exactly the same embed resolves as a list post, and `snapshot.posts`
    // alone would leave its quote, preview and price read forever unresolved.
    for post in snapshot.rendered_posts() {
        let m = Arc::clone(&manager);
        // The buyer's price read (gap (2c)) — independent of the has_media
        // `continue` below, since a sold post's teaser needs it regardless of
        // whether the card also carries media.
        if post.unlock_offer.is_none()
            && post
                .gated_tier
                .as_deref()
                .is_some_and(|t| t.starts_with(fauna_client_subscriptions::UNLOCK_TIER_PREFIX))
        {
            let (m2, id) = (Arc::clone(&manager), post.post_id.clone());
            tokio::spawn(async move { m2.resolve_post_unlock_offer(id).await });
        }
        // The tip surface (`monetization.md` § Tips). No data trigger exists —
        // nothing in the feed projection says whether a post has tips — so the
        // guard is the resolved field alone, and the resolve is fire-once
        // *because* it writes a view on every outcome (an untipped post
        // resolves to zeroes). Ahead of the `has_media` `continue` below for
        // the same reason the price read is: a tipped post owes its surface
        // whether or not the card also carries media.
        #[cfg(feature = "payments")]
        if post.tips.is_none() {
            let (m2, id) = (Arc::clone(&manager), post.post_id.clone());
            tokio::spawn(async move { m2.resolve_post_tips(id).await });
        }
        if post.has_media && post.media_hash.is_none() {
            let id = post.post_id.clone();
            tokio::spawn(async move { m.resolve_media(id).await });
            continue;
        }
        // Either embed field: a QUOTE embeds its target, a REPOST ROW embeds
        // its original — one fold, one resolve (`feed.md` § Interaction bar →
        // Repost; ui.yaml's `quoted-post` is the "quoted/reposted" display).
        if let Some(embedded) = post
            .quoted_post_id
            .as_ref()
            .or(post.reposted_post_id.as_ref())
            && !post.document.has_quoted_post()
        {
            let (m, id) = (Arc::clone(&manager), embedded.clone());
            tokio::spawn(async move { m.resolve_quoted_post(id).await });
        }
        for url in post.document.resolving_link_preview_urls() {
            let (m, url) = (Arc::clone(&manager), url.to_string());
            tokio::spawn(async move { m.resolve_link_preview(url).await });
        }
    }
}

/// Kick a `post-image` byte fetch for every snapshot post whose image hash the
/// cache has not seen — the feed twin of [`crate::media::kick_thumbnail_fetches`].
///
/// [`fire_resolves`] resolves the *hash* (folds `RenderBlock::Image` into the
/// document via `resolve_media`); this resolves the *bytes*. So a post's
/// `first_image_hash()` is only `Some` here once that fold has landed, and the
/// fetch kicks on the next `FeedChanged` tick.
///
/// Idempotent by the cache, not the call site: a hash already `Loading`, `Ready`
/// or `Failed` is filtered out, so a burst of ticks costs one fetch. Returns
/// `None` when there is nothing new (the common tick spawns nothing) or before
/// auth (no `content` plane yet). Marks the requested hashes in flight so the
/// next tick's gate suppresses a duplicate.
pub fn kick_image_fetches(app: &mut App) -> Option<Op> {
    let content = Arc::clone(app.feed.content.as_ref()?);
    // Rides along solely to open a gated post's sealed attachments — see
    // [`Op::FetchImage`]. Before auth there is no manager and no content plane
    // either, so the two `?`s bail together.
    let manager = app.feed.manager.clone()?;
    let snapshot = app.feed.snapshot()?;
    let hashes: Vec<String> = snapshot
        .rendered_posts()
        .flat_map(|post| {
            // Two by-hash sinks share this one fetch: the post's own
            // `post-image`, and every **revealed** link-preview og:image. The
            // og:image is a content-addressed blob this nest fetched and stored
            // (render-model.md § D4), so it is the same `GET /api/v1/blob/<hash>`
            // — not a third-party request. An unrevealed preview yields nothing:
            // the D3/D4 reveal gate is the user's consent, and fetching before it
            // would defeat the gate while still painting nothing.
            post.document
                .first_image_hash()
                .into_iter()
                .chain(
                    post.document
                        .resolved_link_previews()
                        .into_iter()
                        .filter(|preview| preview.revealed)
                        .filter_map(|preview| preview.image_hash),
                )
                .collect::<Vec<_>>()
        })
        .map(|hash| hash.to_string())
        .collect();
    // Marking in the same pass that accepts a hash is what makes the filter
    // dedupe *within* one tick as well as across ticks: two posts sharing an
    // og:image (the same article linked twice) yield that hash twice in one
    // pass, where a single `post-image` per post never could.
    let mut hashes = hashes;
    hashes.retain(|hash| app.feed.images.begin(hash));
    // A bridged post's own pictures (render-model.md § D6c): nest-relative
    // proxied paths, keyed in the same cache by path — a path starts with `/`,
    // so it never collides with a 64-hex hash. They paint immediately: no
    // reveal gate (the user-ruled D6c posture).
    let mut paths: Vec<String> = snapshot
        .rendered_posts()
        .filter_map(|post| post.document.proxied_post_image())
        .map(str::to_string)
        .collect();
    paths.retain(|path| app.feed.images.begin(path));
    if hashes.is_empty() && paths.is_empty() {
        return None;
    }
    Some(Op::FetchImage {
        content,
        manager,
        hashes,
        paths,
    })
}

/// Kick a `doc-remote-image` byte fetch for every **revealed** remote image in
/// the snapshot whose url the cache has not seen — the third-party-host sibling
/// of [`kick_image_fetches`].
///
/// Driven off the same `FeedChanged` tick, which is what makes the reveal paint:
/// `reveal_remote_images` re-emits the snapshot with `revealed: true`, this tick
/// sees urls it did not see before, and the art lands on the tick after that. It
/// deliberately does **not** gate on `content` — this path never touches the nest
/// plane, so it works on a snapshot fetched before the bulk client exists.
pub fn kick_remote_image_fetches(app: &mut App) -> Option<Op> {
    let snapshot = app.feed.snapshot()?;
    let urls: Vec<String> = snapshot
        .posts
        .iter()
        .flat_map(|post| crate::document::revealed_remote_image_urls(&post.document))
        .collect();
    let urls = crate::remote_image::kick(&mut app.feed.remote_images, urls)?;
    Some(Op::FetchRemoteImages { urls })
}

/// Kick a `c2pa-badge` provenance check for every post's own `post-image` hash
/// the cache has not seen — `c2pa-badge`'s twin of [`kick_image_fetches`].
///
/// Scoped to `first_image_hash()` only, never the link-preview og:image: C2PA
/// provenance is a statement about content *this account uploaded*, and an
/// og:image is bytes this nest fetched from a third-party article, never
/// uploaded by the reader (`ui/media.md` § C2PA provenance — every per-app
/// implementation checks only the post's own attachment). Idempotent by the
/// cache, same as the image kick: a hash already in-flight or resolved is
/// filtered out.
pub fn kick_c2pa_fetches(app: &mut App) -> Option<Op> {
    let content = Arc::clone(app.feed.content.as_ref()?);
    // Rides along to open a gated post's sealed attachment before the
    // provenance probe reads it — see [`Op::FetchC2pa`] stage 2, the same
    // reason [`kick_image_fetches`] carries it.
    let manager = app.feed.manager.clone()?;
    let snapshot = app.feed.snapshot()?;
    let mut hashes: Vec<String> = snapshot
        .rendered_posts()
        .filter_map(|post| post.document.first_image_hash())
        .map(|hash| hash.to_string())
        .collect();
    hashes.retain(|hash| app.feed.c2pa.begin(hash));
    if hashes.is_empty() {
        return None;
    }
    Some(Op::FetchC2pa {
        content,
        manager,
        hashes,
    })
}

// ── Elements ────────────────────────────────────────────────────────────────

/// The feed page's ordered element list.
///
/// **Every post in the snapshot gets a card, however few fit the terminal** —
/// the list *is* the automation registry (`crate::element`), so clipping it to
/// the viewport would cap `count("post-card")` at terminal height. The viewport
/// clips paint only.
pub fn elements(app: &App) -> Vec<Element> {
    let mut out = vec![
        Element::label(ids::PAGE_HEADING, feed::list::TITLE),
        Element::label(ids::FEED_VIEW, ""),
    ];
    let Some(snapshot) = app.feed.snapshot() else {
        return out;
    };

    if app.feed.mode == Mode::CreateFeed {
        out.extend(create_feed_elements(
            &app.feed.form,
            &app.settings.trained_topics.rows,
            app.settings.labeler_catalog.subscribed_factors(),
        ));
        return out;
    }
    if let Mode::PostDetail(post_id) = &app.feed.mode {
        // Post detail composes the region source like the list card does
        // (`region-blocking.md` § Where it composes: "no render surface that
        // reaches the composed call for one source while bypassing it for
        // another") — a region-withheld post opened from anywhere shows the
        // placeholder, never the body the card withheld.
        if let Some(withheld) = region_withheld(app, &snapshot, post_id) {
            out.push(Element::label(ids::FEED_POST_DETAIL_DIALOG, ""));
            out.extend(crate::region::placeholder(
                &withheld,
                Gesture::Feed(Action::RevealContent(post_id.clone())),
                Some((ids::FEED_POST_DETAIL_DIALOG, 0)),
            ));
            return out;
        }
        out.extend(post_detail_elements(
            &snapshot,
            post_id,
            &|post: &PostSummary| author_label(app, post),
            &app.feed.images,
            &app.feed.remote_images,
            &app.feed.c2pa,
        ));
        // ui.yaml scopes `image-lightbox` to `feed.post_detail` as well as the
        // list card, and `post_detail_elements` paints a `post-image` too.
        out.extend(lightbox_elements(&app.feed));
        return out;
    }

    // Feed selector + search.
    out.push(
        Element::input(
            ids::FEED_SEARCH_FIELD,
            snapshot.search_query.clone().unwrap_or_default(),
            Field::Feed(FeedField::Search),
        )
        .labelled(feed::post::SEARCH_PLACEHOLDER),
    );
    if snapshot.search_query.is_some() {
        out.push(Element::gesture_button(
            ids::FEED_SEARCH_CLEAR,
            common::CLEAR_SEARCH,
            true,
            Gesture::Feed(Action::ClearSearch),
        ));
    }
    // The built-in Trending virtual feed, **above** the user's own feeds
    // (`trending.md` § The Trending feed; ui.yaml's `feed-trending-item` note).
    // A pseudo-entry, not a `feed-item`: it has no feed row, so it carries no id
    // and never appears in `snapshot.feeds`. Like `feed-item` it paints no
    // selected state — tui's selector shows the selection through the posts it
    // loads, the same shape every other row here uses.
    out.push(Element::gesture_button(
        ids::FEED_TRENDING_ITEM,
        feed::list::TRENDING,
        true,
        Gesture::Feed(Action::SelectTrending),
    ));
    for (i, f) in snapshot.feeds.iter().enumerate() {
        out.push(Element::gesture_button(
            ids::FEED_ITEM,
            f.name.clone(),
            true,
            Gesture::Feed(Action::SelectFeed(f.feed_id.clone())),
        ));
        // Scoped under its own `feed-item` row — the `feed-post-actions-button`
        // idiom under `post-card[i]` — since `feed-delete-button` is itself
        // unindexed (`ui.yaml`).
        out.push(
            Element::gesture_button(
                ids::FEED_DELETE_BUTTON,
                common::DELETE,
                true,
                Gesture::Feed(Action::DeleteFeed(f.feed_id.clone())),
            )
            .within(ids::FEED_ITEM, i),
        );
    }
    out.push(Element::gesture_button(
        ids::FEED_CREATE_FEED_BUTTON,
        feed::post::CREATE_TOOLTIP,
        true,
        Gesture::Feed(Action::OpenCreateFeed),
    ));

    // Subscribed bridge feeds (`feed.md` § Layout & flow region 5) — shown
    // whenever any exist, independent of `available_bridges`: an existing
    // subscription must stay manageable even if that bridge later becomes
    // unavailable. `bridge-feed-unsubscribe-button` is a real INDEXED
    // element (`ui.yaml`), so each paints plain — the `feed-item`/`post-card`
    // idiom for a repeated element's own top-level index.
    for sub in &snapshot.bridge_feeds {
        out.push(Element::chrome(sub.name.clone()));
        out.push(Element::gesture_button(
            ids::BRIDGE_FEED_UNSUBSCRIBE_BUTTON,
            feed::list::UNSUBSCRIBE,
            true,
            Gesture::Feed(Action::UnsubscribeBridge(sub.id)),
        ));
    }
    // The subscribe affordance itself is gated on the nest actually being
    // able to serve at least one bridge (`version-compatibility.md` § Dim
    // 3) — a client must not offer a protocol the nest's build can't serve.
    if !snapshot.available_bridges.is_empty() {
        if snapshot.bridge_feeds.is_empty() {
            out.push(Element::chrome(feed::post::NO_BRIDGE_FEEDS));
        }
        out.push(Element::gesture_button(
            ids::BRIDGE_FEED_SUBSCRIBE_TOGGLE,
            feed::list::SUBSCRIBE_BRIDGE,
            true,
            Gesture::Feed(Action::OpenBridgeForm),
        ));
    }
    // Once open, the form stays painted purely off the local flag — never
    // re-gated on `available_bridges` at paint time (the `Mode::CreateFeed`
    // precedent: a sub-page/overlay doesn't vanish out from under an
    // in-progress edit because unrelated snapshot state changed).
    if app.feed.bridge_form_open {
        out.extend(bridge_form_elements(
            &app.feed.bridge_form,
            &snapshot.available_bridges,
        ));
    }

    // The composer — always inline. `compose-button` is deliberately absent (the
    // `feed-compose-bar` component note sanctions it for always-inline platforms;
    // linux and macOS do the same). It is an omission, not an invisible shim.
    //
    // `compose-dialog-button` is NOT in that omission: unlike `compose-button`
    // (a *toggle* for a collapsible composer, meaningless where compose is
    // always visible), the rich dialog is a second, roomier editing surface —
    // which is why linux and macOS build it despite composing inline too, and
    // why ui.yaml puts `feed-compose-dialog` in the feed page's REQUIRED
    // `elements` rather than `optional_elements`.
    let mut composer = compose_elements(app, &snapshot);
    if app.feed.compose_dialog_open {
        // The dialog IS the composer, relocated — the same ids linux's
        // `build_feed_compose_dialog` puts inside its `adw::Window`
        // (`compose-text-field`, `compose-tags-field`, `compose-file`,
        // `compose-file-ready`, `post-submit-button`), scoped under the dialog
        // so `scope="feed-compose-dialog"` resolves them and an unscoped read
        // still finds exactly one of each.
        out.push(Element::label(ids::FEED_COMPOSE_DIALOG, composer::NEW_POST));
        out.extend(
            composer
                .into_iter()
                .map(|e| e.within(ids::FEED_COMPOSE_DIALOG, 0)),
        );
    } else {
        composer.push(Element::gesture_button(
            ids::COMPOSE_DIALOG_BUTTON,
            feed::post::OPEN_RICH_COMPOSE,
            true,
            Gesture::Feed(Action::OpenComposeDialog),
        ));
        out.extend(composer);
    }

    // Posts, newest-first — `snapshot.posts` IS the visible order. Never sort,
    // filter or paginate client-side (`feed.md` § Anti-patterns); search is a
    // re-query, not a local filter.
    // Empty state: the shared `FeedSnapshot::empty_state` decides whether and
    // which (`feed.md` § Errors & edge cases) — at most one of the two ids,
    // absent while a read is in flight, so loading/empty/populated tell apart
    // off presence alone.
    match snapshot.empty_state() {
        Some(fauna_feed::FeedEmptyState::NoPosts) => {
            out.push(Element::label(ids::FEED_EMPTY_STATE, feed::list::NO_POSTS));
        }
        Some(fauna_feed::FeedEmptyState::NoMatches) => {
            out.push(Element::label(
                ids::FEED_NO_RESULTS,
                feed::list::NO_MATCHING_POSTS,
            ));
        }
        None => {}
    }
    for (i, post) in snapshot.posts.iter().enumerate() {
        // The mute predicate is the SHARED `FeedManager::is_muted` — the sealed
        // `MutedKeywords` scorer the manager installs on a real `fetch_page` —
        // never a second client-side match over the settings list. Two surfaces,
        // two shared seams (the conversations twin runs `body_excludes_matches`
        // post-decrypt because the nest never sees an MLS-sealed body at all).
        let muted = !app.feed.revealed_muted.contains(&post.post_id)
            && app
                .feed
                .manager
                .as_ref()
                .is_some_and(|m| m.is_muted(&post.post_id));
        // Content-policy render enforcement (`family-safety.md` § Content
        // policy): the composed verdict over this post's labels — the viewer's
        // own spam/phishing thresholds, plus a guardian floor when supervised.
        // …and, as the engine's third source, the region policies on the
        // declared chain, with their bundled scorers' factors joined to the
        // post's labels (`region-blocking.md` § Where it composes).
        // …and the viewer's own reports: a post they reported, or whose author
        // they reported, blocks for them alone (`moderation.md` § Corollary).
        let composed = crate::region::verdict_for(
            app,
            &post.post_id,
            &post.labels,
            Some(crate::region::ReportKey {
                item_id: &post.post_id,
                author_id: Some(&post.author),
            }),
            || crate::region::post_input(post),
        );
        let content_verdict = composed.verdict;
        // Guardian Notify (§ Guardian Notify): count this post if the guardian
        // floor enforces on it — a no-op unless the ward's `content_notify` knob
        // is on. Deduped per post per local day (which is what makes it safe to
        // call from a paint a TUI re-runs on every keystroke); the one-minute
        // tick reports the batch.
        app.content_policy
            .note_enforcement(&post.post_id, &post.labels);
        // A `block` floor is absolute — no reveal — so it is checked FIRST,
        // ahead of the muted-keyword collapse, so a post that is both muted
        // (revealable) and blocked can never be revealed past the guardian's
        // block. linux's `post_list.rs` and web's `PostCard.svelte` order the
        // two arms the same way for the same reason.
        // A REGION verdict paints the region's own placeholder — naming the
        // region, its authority and the authority's reason (invariant 1) —
        // rather than the family notice; checked first because it is the same
        // verb, only better attributed. An unrevealed region `collapse` gets its
        // reveal here; once revealed it falls through to the ordinary arms.
        if let Some(withheld) = crate::region::region_verdict(&composed)
            && (withheld.verb == fauna_client_region::RegionVerb::Block
                || !app.feed.revealed_content.contains(&post.post_id))
        {
            out.extend(region_post_card(post, i, &withheld));
        } else if composed.reported() {
            out.extend(reported_post_card(i));
        } else if content_verdict == RenderVerdict::Block {
            out.extend(blocked_post_card(post, i));
        } else if muted {
            out.extend(muted_post_card(post, i));
        } else if content_verdict == RenderVerdict::Collapse
            && !app.feed.revealed_content.contains(&post.post_id)
        {
            // After the muted arm: both are revealable collapses, and a post
            // that is muted should say so (the user's own verb) rather than
            // blaming a content floor.
            out.extend(content_collapsed_post_card(post, i));
        } else {
            out.extend(post_card(
                post,
                author_label(app, post),
                i,
                &app.feed.images,
                &app.feed.remote_images,
                &app.feed.c2pa,
                &snapshot.bridge_roster,
            ));
        }
        // The ⋯ overflow's OPENER is per-card and scoped, because the shared
        // action clicks it with `scope="post-card[i]"`. A collapsed (muted) card
        // gets one too: muting hides what a post SAYS, not the ability to train
        // on it — and the shared action never special-cases a collapsed row.
        out.push(
            Element::gesture_button(
                ids::FEED_POST_ACTIONS_BUTTON,
                // The bare ellipsis glyph, `dm-message-actions-button`'s
                // precedent — an overflow affordance no i18n key exists for
                // (and none is invented for a glyph that needs no translation).
                "\u{2026}",
                true,
                Gesture::Feed(Action::OpenPostActions(post.post_id.clone())),
            )
            .within(ids::POST_CARD, i),
        );
    }
    // The open menu is painted ONCE, flat, after every card — which is exactly
    // what makes the shared action's unscoped verb reads unambiguous ("a closed
    // popover's children are unmapped, so the one open menu is the only match",
    // `actions/feed.py::train_verb_state`).
    out.extend(post_actions_menu(app));
    // Same flat-and-once placement as the ⋯ menu, and for the same reason: at
    // most one attribution window is open, so the shared action reads its rows
    // unscoped.
    #[cfg(feature = "payments")]
    out.extend(tip_list_elements(&app.feed));
    // Same flat-and-once placement again: at most one reply dialog is open.
    out.extend(reply_dialog_elements(&app.feed, &snapshot));
    // The lightbox last — a modal overlay paints over everything below it (the
    // `backup-destination-remove-confirm-modal` placement).
    out.extend(lightbox_elements(&app.feed));
    out
}

/// The open post-card ⋯ overflow, or nothing when no menu is open.
///
/// **Train-in-context.** `FeedManager::train_target_factor` answers `Some` only
/// when the feed's composition has exactly ONE trained factor — then the verbs
/// train it directly. With none (or several) resolved the menu shows
/// `feed-post-train-target-sheet` instead, per ui.yaml's component spec: the
/// verbs would have no unambiguous target, and guessing one would train the
/// wrong model silently.
fn post_actions_menu(app: &App) -> Vec<Element> {
    let Some(post_id) = app.feed.actions_open.clone() else {
        return Vec::new();
    };
    let mut out = vec![Element::label(ids::FEED_POST_ACTIONS_MENU, String::new())];
    out.extend(train_verbs(app, &post_id));
    // ⚠ Deliberately OUTSIDE the training half, which returns early when no
    // single factor composes. The web verbs have nothing to do with training, so
    // sharing that early return would hide the whole family behind an unrelated
    // condition — pinned by
    // `the_web_verbs_paint_with_no_in_context_trained_factor`.
    out.extend(web_publish_verbs(app, &post_id));
    // Last in the menu, and outside the training early return for the same
    // reason the web verbs are: delete has nothing to do with training, so
    // sharing that return would make an own-post's destruction verb vanish on
    // any feed whose composition resolves no single trained factor.
    out.extend(delete_verbs(app, &post_id));
    out.extend(report_verb(app, &post_id));
    out
}

/// `feed-post-report-button` — report someone else's post to the nest admins
/// (`moderation.md` § User-initiated reporting → *App surface*). Gated
/// `!is_own` off the same `PostSummary::author` the delete verb reads, and
/// outside the training early return for the delete verb's reason. Opens the
/// shared report sheet (`crate::report`) with the post as its subject.
fn report_verb(app: &App, post_id: &str) -> Vec<Element> {
    let Some(session) = app.session.as_ref() else {
        return Vec::new();
    };
    let Some(post) = app
        .feed
        .manager
        .as_ref()
        .and_then(|m| m.snapshot().find_post(post_id).cloned())
    else {
        return Vec::new();
    };
    if post.author == session.actor_id {
        return Vec::new();
    }
    vec![Element::gesture_button(
        ids::FEED_POST_REPORT_BUTTON,
        feed::REPORT_POST,
        true,
        Gesture::Report(crate::report::Action::Open(Box::new(
            crate::report::post_target(&post),
        ))),
    )]
}

/// The own-post delete verb and its confirm step (`feed.md` § State & data
/// shape → *Post deletion*, IDs user-approved 2026-07-16).
///
/// **Own posts only**, off the same `PostSummary::author` the web verbs gate on.
/// `fauna.posts.delete` runs three independent author checks nest-side, so this
/// is not the security boundary — it is what stops a verb painting where it
/// could only ever be refused.
///
/// **The two-step is the whole point, and it is the ONE destructive verb here
/// that keeps one.** Unpublish deliberately has none (idempotent, reversible);
/// this destroys the post on the author's nest. The armed step *replaces* the
/// button rather than sitting beside it — linux's shape, which in turn mirrors
/// conversations' `dm-message-delete-button`/`-confirm-button` verbatim, so a
/// second press cannot land on the un-armed verb.
fn delete_verbs(app: &App, post_id: &str) -> Vec<Element> {
    if own_post(app, post_id).is_none() {
        return Vec::new();
    }
    let id = post_id.to_string();
    if app.feed.delete_confirm.as_deref() == Some(post_id) {
        return vec![
            Element::chrome(feed::DELETE_POST_CONFIRM_TITLE),
            Element::gesture_button(
                ids::FEED_POST_DELETE_CONFIRM_BUTTON,
                feed::DELETE_POST_CONFIRM,
                true,
                Gesture::Feed(Action::ConfirmDeletePost(id)),
            ),
        ];
    }
    vec![Element::gesture_button(
        ids::FEED_POST_DELETE_BUTTON,
        feed::DELETE_POST,
        true,
        Gesture::Feed(Action::StartDeletePost(id)),
    )]
}

/// The training half of the ⋯ overflow: both verbs against the in-context
/// factor, or the target sheet when none resolves.
fn train_verbs(app: &App, post_id: &str) -> Vec<Element> {
    let mut out = Vec::new();
    let target = app
        .feed
        .manager
        .as_ref()
        .and_then(|m| m.train_target_factor());
    let Some(factor) = target else {
        // The fallback surface. It is `optional_elements` in ui.yaml, so it may
        // exist without the verbs — and it MUST, or a user on an uncomposed feed
        // would tap ⋯ and get an empty menu.
        out.push(Element::label(
            ids::FEED_POST_TRAIN_TARGET_SHEET,
            feed::TRAIN_TARGET_TITLE,
        ));
        return out;
    };
    let post_id = post_id.to_string();
    // The marker lives in the SEALED model, so it renders the same across
    // restarts and devices — `example_label_for`, never a local set.
    let marked = app
        .feed
        .manager
        .as_ref()
        .and_then(|m| m.example_label_for(&post_id, &factor));
    for (id, verb, label) in [
        (
            "feed-post-more-like-this",
            fauna_feed::TrainVerb::MoreLikeThis,
            feed::MORE_LIKE_THIS,
        ),
        (
            "feed-post-less-like-this",
            fauna_feed::TrainVerb::LessLikeThis,
            feed::LESS_LIKE_THIS,
        ),
    ] {
        // `state` is spelled "on"/"off" here — the vocabulary
        // `actions/feed.py::train_verb_state` compares against. The settings
        // facet's engagement toggle uses "true"/"false"; two action files, two
        // vocabularies, and tui emits no implicit attrs, so an omission reads
        // downstream as a permanently-unmarked post.
        out.push(
            Element::gesture_button(
                id,
                label,
                true,
                Gesture::Feed(Action::TrainPost {
                    post_id: post_id.clone(),
                    verb,
                }),
            )
            .attr("state", if marked == Some(verb) { "on" } else { "off" }),
        );
    }
    out
}

/// The own-post web-publishing verbs (`web-content-hosting.md` § Published-post
/// management; presence rules owned by `ui/feed.md` § User actions).
///
/// **Everything here is state-derived off the post the snapshot already holds**
/// — `PostSummary::{author, web_slug, gated_tier}` — never a per-row query. The
/// publish-state twin of `gated_tier` is `web_slug`: `None` = unpublished.
///
/// Own posts only. `fauna.web.publish.set` is authorship-gated nest-side as
/// well, so this is not the security boundary — but a verb that can only ever
/// refuse is a verb that should never have painted.
///
/// **Both copy affordances disable when the actor has no serving origin**, with
/// the reason painted beside them: publishing with no origin is legal but
/// unreachable, and the doc is explicit that the UI must say so rather than hand
/// out a link that cannot load. The takedown stays live — it needs no origin,
/// and it is the one thing a user with an unreachable site may well want.
fn web_publish_verbs(app: &App, post_id: &str) -> Vec<Element> {
    let Some(session) = app.session.as_ref() else {
        return Vec::new();
    };
    let Some(post) = app
        .feed
        .manager
        .as_ref()
        .and_then(|m| m.snapshot().find_post(post_id).cloned())
    else {
        return Vec::new();
    };
    if post.author != session.actor_id {
        return Vec::new();
    }

    let mut out = Vec::new();
    let id = post_id.to_string();
    let Some(slug) = post.web_slug.clone() else {
        // Unpublished: one verb, and no link affordances for a page that does
        // not exist. A default slug is the nest's to mint, so this needs no
        // origin and no input.
        out.push(Element::gesture_button(
            ids::FEED_POST_PUBLISH_WEB_BUTTON,
            fauna_i18n::strings::web_publish::PUBLISH_TO_WEB,
            true,
            Gesture::Feed(Action::PublishWeb(id)),
        ));
        return out;
    };

    // The origin the copy affordances build on — the same shared resolution the
    // web-settings section uses (active custom domain > enabled subdomain), read
    // from the state that page hydrates, so the two surfaces can never disagree
    // about a creator's address.
    let link = crate::settings::web::site_link(&app.settings);
    let origin = link.origin.as_deref();
    let copied = app
        .feed
        .web_copied
        .as_ref()
        .filter(|c| c.post_id == *post_id);

    // Said once for the menu rather than per verb: the ratified ~10-minute
    // validity and the claim code as the durable alternative. Only when the
    // paywall affordance is actually present below.
    if post.gated_tier.is_some() && origin.is_some() {
        out.push(Element::chrome(
            fauna_i18n::strings::web_publish::PAYWALL_LINK_NOTE,
        ));
    }
    // Why the copy verbs below are dead, in the user's own terms. The ⋯ menu
    // cannot say "the toggle above" — that control is on another page — so the
    // feed's own wording names where to go.
    if origin.is_none() {
        out.push(Element::chrome(
            fauna_i18n::strings::web_publish::MENU_NO_LINK_REASON,
        ));
    }

    out.push(web_copy_button(
        "feed-post-copy-web-link-button",
        fauna_i18n::strings::web_publish::COPY_WEB_LINK,
        origin.map(|o| fauna_client_web::post_page_url(o, &slug)),
        Action::CopyWebLink(id.clone()),
        matches!(copied, Some(c) if matches!(c.kind, CopiedKind::Web)),
        copied,
    ));
    // Gated rows only: an ungated post has no paywalled body, so the mint would
    // hand out a token for nothing.
    if post.gated_tier.is_some() {
        out.push(web_copy_button(
            "feed-post-copy-paywall-link-button",
            fauna_i18n::strings::web_publish::COPY_PAYWALL_LINK,
            // Unlike the public link this value does not exist until the mint
            // round-trips, so the button advertises no `value` up front — only
            // what it actually copied afterwards.
            origin.map(|_| String::new()),
            Action::CopyPaywallLink(id.clone()),
            matches!(copied, Some(c) if matches!(c.kind, CopiedKind::Paywall)),
            copied,
        ));
    }
    out.push(Element::gesture_button(
        ids::FEED_POST_UNPUBLISH_WEB_BUTTON,
        fauna_i18n::strings::web_publish::UNPUBLISH,
        true,
        Gesture::Feed(Action::UnpublishWeb(id)),
    ));

    // What actually went on the clipboard. OSC 52 is fire-and-forget into a
    // terminal that may ignore it, so painting the value is what keeps a
    // clipboard-less terminal from losing the link (the `admin/dns.rs`
    // doctrine) — and for the paywall link it repeats the validity, since that
    // copy is the one with an expiry to remember.
    if let Some(c) = copied {
        out.push(Element::chrome(match c.kind {
            CopiedKind::Web => fauna_i18n::strings::web_publish::copied_link(&c.url),
            CopiedKind::Paywall => fauna_i18n::strings::web_publish::copied_paywall_link(&c.url),
        }));
    }
    out
}

/// A snapshot post id (hex) as the wire's `post_id` bytes.
///
/// Fallible on purpose rather than a silent `unwrap_or_default`: publishing
/// against an empty id would ask the nest to serve a post that does not exist,
/// and the user would see a takedown verb for a page nobody can reach.
fn decode_post_id(post_id: &str) -> Result<Vec<u8>, String> {
    hex::decode(post_id).map_err(|e| format!("unreadable post id {post_id:?}: {e}"))
}

/// The post a web verb names, if it is still the viewer's own and still in the
/// publish state the verb was painted for.
///
/// Re-resolved at click time rather than captured at paint: the loaded window
/// re-ranks in place, so a verb clicked a frame late can otherwise address a
/// post the user never saw it on. `want_published` distinguishes the takedown
/// and copy verbs (which need a live slug) from publish (which needs none).
fn own_published_post(
    app: &App,
    post_id: &str,
    want_published: bool,
) -> Option<fauna_feed::PostSummary> {
    let session = app.session.as_ref()?;
    // Owned: `snapshot()` hands back a guard, so a borrow into it cannot outlive
    // this call.
    let post = app
        .feed
        .manager
        .as_ref()?
        .snapshot()
        .find_post(post_id)
        .cloned()?;
    (post.author == session.actor_id && post.web_slug.is_some() == want_published).then_some(post)
}

/// The post `post_id` names, if it is still in the loaded window AND still this
/// actor's own — [`own_published_post`] without the publish-state half, for the
/// delete verbs, which care about authorship and nothing else.
///
/// `fauna.posts.delete` is authorship-gated nest-side by three independent
/// checks, so this is not the security boundary; it is what stops a verb
/// painting (or firing) where it could only ever be refused.
fn own_post(app: &App, post_id: &str) -> Option<fauna_feed::PostSummary> {
    let session = app.session.as_ref()?;
    let post = app
        .feed
        .manager
        .as_ref()?
        .snapshot()
        .find_post(post_id)
        .cloned()?;
    (post.author == session.actor_id).then_some(post)
}

/// One copy affordance in the ⋯ menu: disabled (with no `value`) when there is
/// no serving origin, and carrying the exact string it put on the clipboard once
/// it has fired.
///
/// The `copied` attr is what lets a test assert the copied **contents** rather
/// than the mere presence of a button — the devices-page lesson that unasserted
/// copy affordances rot invisibly. It is written from the same value that
/// reached `copy_to_clipboard`, never re-derived, so the two cannot drift.
fn web_copy_button(
    id: &str,
    label: &str,
    value: Option<String>,
    action: Action,
    is_last_copied: bool,
    copied: Option<&CopiedFeedLink>,
) -> Element {
    let el = Element::gesture_button(id, label, value.is_some(), Gesture::Feed(action))
        .attr("value", value.unwrap_or_default());
    match (is_last_copied, copied) {
        (true, Some(c)) => el.attr("copied", c.url.clone()),
        _ => el,
    }
}

fn compose_elements(app: &App, snapshot: &FeedSnapshot) -> Vec<Element> {
    let compose = &snapshot.compose;
    let mut out = vec![
        Element::input(
            ids::COMPOSE_TEXT_FIELD,
            compose.text.clone(),
            Field::Feed(FeedField::ComposeText),
        )
        .labelled(feed::post::WHATS_ON_YOUR_MIND),
        Element::input(
            ids::COMPOSE_TAGS_FIELD,
            compose.tags.clone(),
            Field::Feed(FeedField::ComposeTags),
        )
        .labelled(feed::post::TAGS_PLACEHOLDER),
        // A path input, not an OS file picker — `tui.md` § Declared platform
        // absences 4. The e2e driver reaches the same state through the
        // `compose.file` patch, so both paths stage one value.
        Element::input(
            ids::COMPOSE_FILE,
            app.feed.staged_file.clone().unwrap_or_default(),
            Field::Feed(FeedField::ComposeFile),
        )
        .labelled(feed::post::ATTACH_IMAGE),
    ];
    // The attached file off the manager's snapshot, never off the local path: a
    // draft restored after a relaunch, or synced from another device, carries a
    // handle with no path behind it, and its submit refuses naming exactly this
    // file (`feed.md` § Persistence → *Attachments by content address*). Name
    // and size is `dm-compose-attachment-chip`'s shape one page over.
    if let Some(file) = &compose.attached_file {
        out.push(Element::label(
            ids::COMPOSE_FILE_READY,
            format!(
                "{} ({})",
                file.name,
                crate::wizard::localized(&fauna_core::format::byte_size(file.size))
            ),
        ));
        out.push(Element::gesture_button(
            ids::COMPOSE_FILE_REMOVE,
            common::REMOVE,
            !compose.submitting,
            Gesture::Feed(Action::RemoveComposeAttachment),
        ));
    }

    // The gate select — Public, each of the author's own tiers, then "Sell this
    // post…" (`ui/feed.md` § Encryption at rest; `monetization.md` § Pillars
    // 2+3 and § Per-post pay-to-unlock). `own_tiers` is refreshed by the shared
    // manager alongside the feed itself and **already excludes designated
    // unlock tiers** (`refresh_own_tiers`), so a previously sold post's tier
    // can never show up here as a gate target.
    // Then one option per room the author can address a post to (`own_rooms`,
    // refreshed by the same manager read), named "Room: <its label>".
    let room_option = |room: &str| {
        snapshot
            .own_rooms
            .iter()
            .find(|r| r.room == room)
            .map(|r| feed::post::gate_room(&r.label))
    };
    let mut options = vec![feed::post::GATE_PUBLIC.to_string()];
    options.extend(snapshot.own_tiers.iter().map(|t| t.name.clone()));
    options.extend(
        snapshot
            .own_rooms
            .iter()
            .map(|r| feed::post::gate_room(&r.label)),
    );
    // "Sell this post…" is the paywall-designation gesture, the money plane's
    // author half (`dynamic-features.md` § Platform-family surface excision →
    // *The price-and-route class*): a store-safe build offers no sale and
    // paints none of the sell controls below. A sale staged on a full client
    // and synced in is not this build's to show — its select reads as the
    // gated answer the shared state otherwise carries.
    #[cfg(feature = "payments")]
    options.push(feed::post::GATE_SELL.to_string());
    #[cfg(feature = "payments")]
    let selling = compose.sell.is_some();
    #[cfg(not(feature = "payments"))]
    let selling = false;
    let selected = if selling {
        feed::post::GATE_SELL.to_string()
    } else if let Some(room) = compose.gate_room.as_deref().and_then(room_option) {
        room
    } else {
        compose
            .gate_tier
            .clone()
            .unwrap_or_else(|| feed::post::GATE_PUBLIC.to_string())
    };
    out.push(
        Element::select(
            ids::COMPOSE_GATE_TIER_SELECT,
            selected,
            SelectTarget::ComposeGateTier,
            options,
        )
        .labelled(feed::post::GATE_AUDIENCE),
    );

    // The teaser: rendered for EVERY restricted answer, because each publishes a
    // gated post whose public body is this text — and each submit rejects it
    // empty.
    let gated =
        compose.gate_tier.is_some() || compose.sell.is_some() || compose.gate_room.is_some();
    if gated {
        out.push(
            Element::input(
                ids::COMPOSE_GATE_PREVIEW_FIELD,
                compose.gate_preview.clone(),
                Field::Feed(FeedField::ComposeGatePreview),
            )
            .labelled(feed::post::GATE_PREVIEW_PLACEHOLDER),
        );
    }
    // The sell controls, visible only in sell mode — the money plane's, with
    // the answer that reveals them (above).
    #[cfg(feature = "payments")]
    if let Some(sell) = &compose.sell {
        out.push(
            Element::input(
                ids::COMPOSE_SELL_PRICE,
                sell.price.clone(),
                Field::Feed(FeedField::ComposeSellPrice),
            )
            .labelled(feed::post::SELL_PRICE_PLACEHOLDER),
        );
        out.push(
            Element::input(
                ids::COMPOSE_SELL_ASKING_PRICE,
                sell.asking_price.clone(),
                Field::Feed(FeedField::ComposeSellAskingPrice),
            )
            .labelled(feed::post::SELL_ASKING_PRICE_PLACEHOLDER),
        );
        out.push(Element::checkbox_gesture(
            ids::COMPOSE_SELL_SUBSCRIBERS_FREE,
            feed::post::SELL_SUBSCRIBERS_FREE,
            sell.subscribers_get_it_free,
            Gesture::Feed(Action::ToggleSellSubscribersFree),
        ));
    }

    if let Some(error) = &compose.error {
        out.push(Element::label(
            ids::COMPOSE_ERROR,
            crate::wizard::localized(error),
        ));
    }
    out.push(Element::gesture_button(
        ids::POST_SUBMIT_BUTTON,
        common::POST,
        !compose.submitting,
        Gesture::Feed(Action::SubmitPost),
    ));
    out
}

/// The `post-image` element for a resolved image `hash`: the rasterized art once
/// its bytes have loaded ([`kick_image_fetches`] → [`Op::FetchImage`]), else the
/// hash as a placeholder label until then / on a fetch failure. Same registry id
/// either way, so the element is always addressable; the painted half-block `▀`
/// text is the e2e paint observable, exactly like Media's `media-thumbnail`.
fn post_image_element(hash: &str, images: &ImageCache) -> Element {
    let element = match images.get(hash) {
        Some(ImageState::Ready(art)) => Element::thumbnail(ids::POST_IMAGE, art.clone()),
        Some(ImageState::Loading) | Some(ImageState::Failed) | None => {
            Element::label(ids::POST_IMAGE, hash.to_string())
        }
    };
    // `post-image` is a BUTTON on every app that has one — linux's
    // `build_post_image` returns a `gtk::Button`, web's `C2paImage` takes an
    // `onclick`, apple taps it in `PostCardView` — and what the click does is
    // open `image-lightbox`. Clickable in every load state, so the role (and so
    // the focus ring) does not flicker as bytes arrive; the lightbox paints
    // whatever the cache holds, placeholder included.
    element.clickable(Gesture::Feed(Action::OpenLightbox(hash.to_string())))
}

/// The `video-thumbnail` element for a folded [`RenderBlock::Video`] hash — or a
/// bridged [`RenderBlock::ProxiedVideo`](fauna_core::render::RenderBlock::ProxiedVideo)'s
/// proxied path (render-model.md § D6c → *Proxied video*) — (`ui.yaml` `video-thumbnail`,
/// inside `post-card`) — the video twin of [`post_image_element`], painted from the typed
/// block the shared fold now emits (render-model.md § Implementation status today).
///
/// Text is the play glyph + the hash (or path), so the element is addressable and carries a
/// paint observable in every load state, exactly like the image placeholder.
///
/// **Not clickable, and deliberately not byte-loaded.** Inline AV playback is one of tui's
/// four declared platform absences (`tui.md` § Declared platform absences), so there is no
/// gesture to attach; and a *poster* frame would be what a still preview needs, which no
/// writer populates today (`MediaItem::thumbnail` is `None` everywhere — see
/// `RenderBlock::Video`'s own note). Rasterizing the video blob itself as an image is exactly
/// the confusion the typed variant exists to prevent. Painting the marker is real parity here:
/// `video-thumbnail` is NOT among tui's declared absences, and before this a video post
/// painted a permanently-failed `post-image` placeholder instead.
fn video_thumbnail_element(hash_or_path: &str) -> Element {
    Element::label(ids::VIDEO_THUMBNAIL, format!("▶ {hash_or_path}"))
}

/// The `link-preview-image` element for a **revealed** og:image `hash`: the
/// rasterized art once its bytes have loaded, else the hash as a placeholder
/// label. The `post_image_element` shape, minus the lightbox gesture — no app
/// makes the og:image a lightbox trigger (the *card* is what carries the click,
/// and it opens the url), so making tui's clickable would be a per-app
/// divergence rather than parity.
///
/// Only ever called under `preview.revealed`: the reveal gate lives at the call
/// site, so this helper can never paint blocked content by accident.
fn og_image_element(hash: &str, images: &ImageCache) -> Element {
    match images.get(hash) {
        Some(ImageState::Ready(art)) => Element::thumbnail(ids::LINK_PREVIEW_IMAGE, art.clone()),
        Some(ImageState::Loading) | Some(ImageState::Failed) | None => {
            Element::label(ids::LINK_PREVIEW_IMAGE, hash.to_string())
        }
    }
}

/// The post tip surface — `post-tip-total` / `post-tip-count` /
/// `post-tip-list-button` (`monetization.md` § Tips, IDs user-approved
/// 2026-08-11), for whichever surface is painting the post.
///
/// **The two counters are guarded independently, and that is the whole point.**
/// `tip_count` counts every tip; `total_msats` sums only those whose receipt
/// reported an amount. So a post all of whose receipts carried an unparseable
/// invoice renders "3 tips" and **no amount at all** — rendering "0 sats" there
/// would tell the reader nobody paid, which is false. The nest's own wire doc
/// says the same in as many words ("this can exceed the number of tips
/// contributing to `total_msats`"), and § Tips forbids coercing a missing
/// amount to 0.
///
/// `None` tips = not resolved yet (`fire_resolves` asks once per post); an
/// untipped post resolves to zeroes and paints nothing here, and so does a
/// failed read — one empty surface for all three, the ratified
/// degradation.
/// `payments`-gated even though `PostSummary.tips` is an ungated inert record
/// that stays `None` forever in a store-safe build. Inertness makes the surface
/// DEAD; it does not make it ABSENT, and criterion 1 of `dynamic-features.md`
/// § What "completely compiled away" means is a `strings`-grep for element ids.
/// This is the same trap `TipView` sprang on `ffi-store-safe-check` (2026-08-11)
/// — red with no sender anywhere in the graph.
#[cfg(feature = "payments")]
fn tip_elements(post: &PostSummary, scope: Option<(&str, usize)>) -> Vec<Element> {
    let Some(tips) = &post.tips else {
        return Vec::new();
    };
    if tips.tip_count == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    // The amount, only when something was actually summable.
    if tips.total_msats != 0 {
        out.push(Element::label(
            ids::POST_TIP_TOTAL,
            crate::format::tip_amount(tips.total_msats),
        ));
    }
    out.push(Element::label(
        ids::POST_TIP_COUNT,
        crate::format::tip_count(tips.tip_count),
    ));
    out.push(Element::gesture_button(
        ids::POST_TIP_LIST_BUTTON,
        tips_i18n::LIST_OPEN,
        true,
        Gesture::Feed(Action::OpenTipList(post.post_id.clone())),
    ));
    match scope {
        Some((parent, i)) => out.into_iter().map(|e| e.within(parent, i)).collect(),
        None => out,
    }
}

/// The `post-tip-list` attribution window, or nothing when none is open.
///
/// Painted flat like the lightbox — there is exactly one open at a time, and
/// the shared action reads it unscoped.
///
/// **Every row the nest sent is rendered, unfiltered.** A tip's authenticity is
/// settled at ingest and never at read (`monetization.md` § Zap receipts), so a
/// client-side trust check here would re-open exactly the per-reader
/// re-checking that discipline exists to prevent.
#[cfg(feature = "payments")]
fn tip_list_elements(state: &FeedState) -> Vec<Element> {
    let (Some(post_id), Some(snapshot)) = (&state.tip_list_open, state.snapshot()) else {
        return Vec::new();
    };
    let Some(tips) = snapshot
        .rendered_posts()
        .find(|p| &p.post_id == post_id)
        .and_then(|p| p.tips.as_ref())
    else {
        return Vec::new();
    };
    // The bounded window's tail, from the nest's own `has_more` — never
    // inferred by comparing the row count against a cap this client hard-codes
    // (the wire carries the flag precisely so no client has to).
    let title = if tips.has_more {
        let more = tips.tip_count - tips.senders.len() as i64;
        format!(
            "{} — {}",
            tips_i18n::LIST_TITLE,
            crate::format::tip_more(more)
        )
    } else {
        tips_i18n::LIST_TITLE.to_string()
    };
    let mut out = vec![Element::label(ids::POST_TIP_LIST, title)];
    for tip in &tips.senders {
        // Who: the local actor when the mechanism identity resolved to one,
        // else the mechanism-native id it published, else the localized
        // stand-in — an outside tip still counts and still displays.
        let who = tip
            .sender
            .as_deref()
            .or(tip.sender_ref.as_deref())
            .unwrap_or(tips_i18n::SENDER_UNKNOWN);
        // How much, or the honest absence. NEVER "0 sats".
        let amount = match tip.amount_msats {
            Some(msats) => crate::format::tip_amount(msats),
            None => tips_i18n::AMOUNT_UNKNOWN.to_string(),
        };
        out.push(Element::label(
            ids::POST_TIP_ITEM,
            format!("{who} — {amount}"),
        ));
    }
    out
}

/// The `image-lightbox` overlay: the open post image, re-rasterized from the
/// cached pixels at [`crate::thumbnail::LIGHTBOX_COLS`], or nothing when no
/// image is open.
///
/// A **full-screen viewer** is what ui.yaml's component asks for, and on a
/// terminal that is a cell grid several times the inline card preview. The
/// bytes are long gone by now (they are dropped when the fetch op completes), so
/// the bigger art comes from [`crate::thumbnail::rasterize_rgb`] over the
/// cached [`crate::thumbnail::Thumbnail::pixels`] — no second GET.
///
/// Painted flat, not scoped: the test reads `is_visible("image-lightbox")`
/// unscoped, and there is exactly one at a time.
fn lightbox_elements(state: &FeedState) -> Vec<Element> {
    let Some(hash) = &state.lightbox else {
        return Vec::new();
    };
    // A picture whose bytes never arrived still opens — as the placeholder the
    // card showed. The alternative (a click that silently does nothing while
    // the fetch is in flight) is the drop-an-action shape testing.md point 11
    // forbids.
    let enlarged = match state.images.get(hash) {
        Some(ImageState::Ready(thumb)) => {
            crate::thumbnail::rasterize_rgb(&thumb.pixels, crate::thumbnail::LIGHTBOX_COLS)
        }
        _ => None,
    };
    vec![match enlarged {
        Some(thumb) => Element::thumbnail(ids::IMAGE_LIGHTBOX, thumb),
        None => Element::label(ids::IMAGE_LIGHTBOX, crate::thumbnail::PLACEHOLDER),
    }]
}

/// The `feed-reply-dialog` overlay — the armed [`FeedState::reply_draft`], or
/// nothing while none is open. Painted flat, not scoped, the same posture as
/// [`tip_list_elements`]/[`lightbox_elements`] above: at most one is open.
///
/// Mirrors the shared cross-app shape (linux's `build_reply_dialog`, web's
/// reply overlay): a title naming the target's author, one text field, one
/// submit button — the dialog IS the composer, relocated, the same
/// `.within(dialog_id, 0)` scoping `feed-compose-dialog` uses above. No
/// dismiss element (ui.yaml registers none): Escape is the human-only
/// affordance, wired in `app.rs` beside `compose_dialog_open`'s.
fn reply_dialog_elements(state: &FeedState, snapshot: &FeedSnapshot) -> Vec<Element> {
    let Some(draft) = &state.reply_draft else {
        return Vec::new();
    };
    let target = snapshot.find_post(&draft.post_id);
    let author = target.map(|p| p.author.clone()).unwrap_or_default();
    let mut out = vec![Element::label(
        ids::FEED_REPLY_DIALOG,
        feed::post::replying_to_user(&author),
    )];
    // Under a restricted target the dialog STATES where the reply goes —
    // `feed-reply-audience`, the manager's own per-post answer, never a
    // decision made here — and offers the explicit-public answer
    // (`feed-reply-public-confirm`) only where this reader cannot write for
    // the audience (`ui/feed.md` § Encryption at rest → *Ruling 5's build —
    // the shape*, (d) + (e)). A public target paints neither.
    let audience = target.and_then(|p| p.reply_audience);
    if let Some(target) = target
        && let Some(audience) = audience
    {
        let text = match audience {
            ReplyAudience::SealedToRoom => {
                feed::post::reply_audience_room(target.room_label.as_deref().unwrap_or_default())
            }
            ReplyAudience::SealedToTier => {
                feed::post::reply_audience_tier(target.gated_tier.as_deref().unwrap_or_default())
            }
            ReplyAudience::PublicByConfirmation => feed::post::REPLY_AUDIENCE_PUBLIC.to_string(),
        };
        out.push(Element::label(ids::FEED_REPLY_AUDIENCE, text).within(ids::FEED_REPLY_DIALOG, 0));
    }
    out.push(
        Element::input(
            ids::FEED_REPLY_TEXT_FIELD,
            draft.text.clone(),
            Field::Feed(FeedField::ReplyText),
        )
        .labelled(feed::post::WRITE_REPLY)
        .within(ids::FEED_REPLY_DIALOG, 0),
    );
    if audience == Some(ReplyAudience::PublicByConfirmation) {
        out.push(
            Element::checkbox_gesture(
                ids::FEED_REPLY_PUBLIC_CONFIRM,
                feed::post::REPLY_PUBLIC_CONFIRM,
                draft.public_confirmed,
                Gesture::Feed(Action::ToggleReplyPublicConfirm),
            )
            .within(ids::FEED_REPLY_DIALOG, 0),
        );
    }
    out.push(
        Element::gesture_button(
            ids::FEED_REPLY_SUBMIT_BUTTON,
            common::REPLY,
            !draft.text.trim().is_empty(),
            Gesture::Feed(Action::SubmitReply),
        )
        .within(ids::FEED_REPLY_DIALOG, 0),
    );
    out
}

/// The muted-keyword collapse for one post: the placeholder + its one-tap
/// reveal, in place of the whole card body.
///
/// `post-card` still registers (it is the focus-ring stop and the bottom-of-feed
/// `load_more` trigger), but its **text is the placeholder, not the body** — a
/// collapsed post must not leak what it says, and the card's text is what the
/// harness's find-a-post-by-its-text helper reads. `feed-post-text` and every
/// embed are simply absent, which is what makes `count("feed-post-text")` — the
/// read behind `actions/feed.py::post_count` — naturally exclude a collapsed
/// post, exactly as it does on the other six apps.
fn muted_post_card(post: &PostSummary, i: usize) -> Vec<Element> {
    vec![
        Element::gesture_button(
            ids::POST_CARD,
            feed::POST_MUTED_PLACEHOLDER,
            true,
            Gesture::Feed(Action::OpenPostDetail(post.post_id.clone())),
        ),
        Element::label(ids::FEED_POST_MUTED, feed::POST_MUTED_PLACEHOLDER)
            .within(ids::POST_CARD, i),
        Element::gesture_button(
            ids::FEED_POST_MUTED_REVEAL_BUTTON,
            feed::POST_MUTED_REVEAL,
            true,
            Gesture::Feed(Action::RevealMuted(post.post_id.clone())),
        )
        .within(ids::POST_CARD, i),
    ]
}

/// A post the content policy **blocks** (`family-safety.md` § Content policy):
/// the placeholder names the policy and there is **no reveal** — a `block` floor
/// is absolute.
///
/// Painted exactly like [`muted_post_card`] otherwise: `post-card` still
/// registers (focus-ring stop, `load_more` trigger) but its text is the
/// placeholder, so a blocked post cannot leak what it says and
/// `count("feed-post-text")` naturally excludes it, as on the other six apps.
/// `content-policy-blocked-notice` is the ui.yaml id (user-approved 2026-07-15)
/// the e2e drives.
fn blocked_post_card(post: &PostSummary, i: usize) -> Vec<Element> {
    vec![
        Element::gesture_button(
            ids::POST_CARD,
            family::CONTENT_BLOCKED_NOTICE,
            true,
            Gesture::Feed(Action::OpenPostDetail(post.post_id.clone())),
        ),
        Element::label(
            ids::CONTENT_POLICY_BLOCKED_NOTICE,
            family::CONTENT_BLOCKED_NOTICE,
        )
        .within(ids::POST_CARD, i),
    ]
}

/// A post the viewer **reported** — or whose author they reported
/// (`moderation.md` § Corollary — block also hides): the render engine's
/// `Block` attributed `Reported`, painted as the "You reported this"
/// placeholder in the same `content-policy-blocked-notice` slot every other
/// engine `Block` uses (`source="reported"` tells the two apart). The card is
/// a plain label, not a detail door: opening it would show exactly what the
/// reporter asked not to see again.
fn reported_post_card(i: usize) -> Vec<Element> {
    vec![
        Element::label(
            ids::POST_CARD,
            fauna_i18n::strings::moderation::report::HIDDEN_PLACEHOLDER,
        ),
        Element::label(
            ids::CONTENT_POLICY_BLOCKED_NOTICE,
            fauna_i18n::strings::moderation::report::HIDDEN_PLACEHOLDER,
        )
        .attr("source", "reported")
        .within(ids::POST_CARD, i),
    ]
}

/// The region verb withholding `post_id` right now — a `block`, or a
/// `collapse` not yet revealed this session — with its attribution.
fn region_withheld(
    app: &App,
    snapshot: &FeedSnapshot,
    post_id: &str,
) -> Option<fauna_client_region::RegionPlaceholder> {
    let post = snapshot.find_post(post_id)?;
    let composed = crate::region::verdict_for(app, &post.post_id, &post.labels, None, || {
        crate::region::post_input(post)
    });
    let withheld = crate::region::region_verdict(&composed)?;
    (withheld.verb == fauna_client_region::RegionVerb::Block
        || !app.feed.revealed_content.contains(&post.post_id))
    .then_some(withheld)
}

/// How many posts the feed page's current surface REGION-blocks — the verdict
/// side of the convention-17 "a region Block never renders silent" invariant
/// (`crate::region::block_render_json`). Walks the posts the page paints (the
/// whole list, or the one open in detail) through the same
/// `crate::region::verdict_for`, never through the paint's arms.
pub(crate) fn region_blocked_count(app: &App) -> usize {
    let Some(snapshot) = app.feed.snapshot() else {
        return 0;
    };
    let is_blocked = |post: &PostSummary| {
        let composed = crate::region::verdict_for(app, &post.post_id, &post.labels, None, || {
            crate::region::post_input(post)
        });
        crate::region::region_verdict(&composed)
            .is_some_and(|p| p.verb == fauna_client_region::RegionVerb::Block)
    };
    match &app.feed.mode {
        Mode::List => snapshot.posts.iter().filter(|p| is_blocked(p)).count(),
        Mode::PostDetail(id) => snapshot
            .find_post(id)
            .map_or(0, |p| usize::from(is_blocked(p))),
        Mode::CreateFeed => 0,
    }
}

/// A post a **region** policy blocks or collapses (`region-blocking.md` § The
/// blocked render and the transparency surface): the placeholder in place of
/// the body, scoped in `post-card[i]`. The card itself is a plain label — a
/// region-withheld post has no detail to open (detail does not re-run the
/// verdict, so opening it would show exactly what the region withheld).
fn region_post_card(
    post: &PostSummary,
    i: usize,
    withheld: &fauna_client_region::RegionPlaceholder,
) -> Vec<Element> {
    let mut els = vec![Element::label(
        ids::POST_CARD,
        crate::region::notice_text(withheld),
    )];
    els.extend(crate::region::placeholder(
        withheld,
        Gesture::Feed(Action::RevealContent(post.post_id.clone())),
        Some((ids::POST_CARD, i)),
    ));
    els
}

/// A post the content policy **collapses** — the viewer's own threshold or a
/// guardian `collapse` floor. One-tap reveal, session-local; the floor itself
/// persists.
///
/// The placeholder and the reveal are untagged ([`Element::chrome`] /
/// [`Element::chrome_button`]): ui.yaml scopes no id to this arm, and both
/// sibling implementations (linux's `build_content_collapse`, web's
/// `PostCard.svelte`) made the same call — "v1 e2e drives the block case".
fn content_collapsed_post_card(post: &PostSummary, i: usize) -> Vec<Element> {
    vec![
        Element::gesture_button(
            ids::POST_CARD,
            family::CONTENT_COLLAPSED_NOTICE,
            true,
            Gesture::Feed(Action::OpenPostDetail(post.post_id.clone())),
        ),
        Element::chrome(family::CONTENT_COLLAPSED_NOTICE).within(ids::POST_CARD, i),
        Element::chrome_button(
            family::CONTENT_REVEAL_BUTTON,
            Gesture::Feed(Action::RevealContent(post.post_id.clone())),
        )
        .within(ids::POST_CARD, i),
    ]
}

/// What the feed calls a post's author: the one shared resolver
/// (`fauna_core::format::peer_display_label`, value-formatting.md § Peer
/// display label) over the viewer's own nickname for them (contacts.md § The
/// private overlay). A **bridged** author's face — the display name and handle
/// the origin bridge served, `PostSummary.author_display` (bridges.md
/// § Unified feed ingestion → *Bridged authors*) — fills the resolver's
/// self-published-name and handle slots, so the chain reads nickname → display
/// name → handle → short id; a native author carries neither today and reads
/// as the canonical short id.
fn author_label(app: &App, post: &PostSummary) -> String {
    let nickname = crate::contacts::overlay_projection(app).and_then(|p| p.nickname(&post.author));
    let face = post.author_display.as_ref();
    fauna_core::format::peer_display_label(
        nickname.as_deref(),
        face.and_then(|f| f.display_name.as_deref()),
        face.and_then(|f| f.handle.as_deref()),
        &post.author,
    )
    .primary
}

/// One post card, with its children scoped under `post-card[i]`.
///
/// The children carry the **same literal id** repeated per card, addressed by
/// ancestor scope — `count("tag-chip", scope="post-card[2]")`. That is the one
/// convention the shared feed suites use; it is NOT `post-card-{i}` (a
/// dash-index id) and NOT `post-card[{i}]` as a literal id string. Registering
/// the wrong one leaves every scoped query resolving to nothing.
fn post_card(
    post: &PostSummary,
    author: String,
    i: usize,
    images: &ImageCache,
    remote_images: &ImageCache,
    c2pa: &C2paCache,
    bridges: &[fauna_core::source_glyph::BridgeIdentitySnapshot],
) -> Vec<Element> {
    // A REPOST ROW (`feed.md` § Interaction bar → Repost, ratified 2026-08-10):
    // attribution + the embedded original, and **activation opens the
    // ORIGINAL's detail** — the repost post itself is empty by construction, so
    // its own detail would be the blank-dialog class this design killed.
    let is_repost_row = post.reposted_post_id.is_some();
    let detail_target = post
        .reposted_post_id
        .clone()
        .unwrap_or_else(|| post.post_id.clone());
    let mut out = vec![
        // `text` is the body plaintext so the harness's find-a-post-by-its-text
        // helper can locate a card; clicking it opens `post_detail` (ui.yaml
        // `feed` transition `click post-card → post_detail`), which is also what
        // makes the card a focus-ring stop so the ring can reach the bottom of
        // the feed (which is what triggers `load_more`).
        //
        // It also heads the card the engagement-cue capture measures (only this
        // arm — the muted/blocked/collapsed placeholders hide what the post
        // says, so lingering on one is no exposure to it). The row's OWN post id,
        // a repost row's included, as linux stamps its row.
        Element::gesture_button(
            ids::POST_CARD,
            post.document.to_plaintext(),
            true,
            Gesture::Feed(Action::OpenPostDetail(detail_target)),
        )
        .cue(cues::CueSubject {
            post_id: post.post_id.clone(),
            is_media: post.has_media,
        }),
        Element::label(ids::POST_AUTHOR, author).within(ids::POST_CARD, i),
        // The document, walked. The registry still reads its plaintext, which is
        // what every app's `feed-post-text` reports. For a repost row this is
        // the folded embed alone (the body is empty by construction).
        Element::document(ids::FEED_POST_TEXT, post.document.clone()).within(ids::POST_CARD, i),
    ];

    // The attribution marker beside the author. The `repost-attribution` id was
    // user-approved 2026-08-11, so this is a real element now rather than the
    // un-id'd chrome it shipped as: it is what lets an e2e tell a repost card
    // from an empty-commentary quote card BY ELEMENT, instead of inferring it
    // from the harness state dump's `reposted_post_id`.
    if is_repost_row {
        out.push(
            Element::label(
                ids::REPOST_ATTRIBUTION,
                format!("⇄ {}", fauna_i18n::strings::feed::post::REPOSTED_MARKER),
            )
            .within(ids::POST_CARD, i),
        );
    }

    // `security.md` § App display of unverified content: a signed envelope
    // THIS client could not verify gets the muted caveat badge, never a
    // vanished post (a key-rotation-lag false negative must not hide content).
    if post.verification == VerificationStatus::Failed {
        out.push(
            Element::label(ids::UNVERIFIED_SOURCE_BADGE, feed::UNVERIFIED_SOURCE)
                .within(ids::POST_CARD, i),
        );
    }

    // The D10 audit surface (`atproto-pds-full.md` § Problem 1 -> D10 ->
    // *Audit*, ratified 2026-07-29): the badge is how a user reading their own
    // feed can tell which posts an EXTERNAL APP wrote as them. The signed bytes
    // are the log, so this is a client-side read of what the account itself
    // authorized — never a nest-reported claim.
    //
    // Keyed on `Delegated` alone: `Unknown` covers both the undecoded list card
    // and the FAILED-verification case, and the latter is the one that matters —
    // an unverified wire's `signer_auth` cert is precisely what nothing
    // authenticated, so badging it would let a forgery describe its own origin.
    if post.authoring_origin == AuthoringOriginStatus::Delegated {
        out.push(
            Element::label(ids::DELEGATED_ORIGIN_BADGE, feed::DELEGATED_ORIGIN)
                .within(ids::POST_CARD, i),
        );
    }

    // `content-label-badge` — the highest-confidence classifier verdict on this
    // post, via the SAME shared pair the moderation queue uses
    // (`primary_content_label` picks the entry, `content_label_style` styles it),
    // so a post carries an identical badge wherever it renders
    // (`moderation.md` § Per-row badge data path). `PostSummary.labels` is
    // projected nest-side by `query_feed`; an empty list paints nothing.
    if let Some(badge) = crate::moderation::content_label_badge(&post.labels) {
        out.push(badge.within(ids::POST_CARD, i));
    }

    // Gated-to-tier badge (`gated-post-badge`, `ui/feed.md` § Encryption at
    // rest) — the tier name, on every gated post's card, exactly as linux paints
    // it (`views/feed/post_list.rs:1135`). The list body stays the plaintext
    // teaser either way; this badge is the only thing that tells a reader the
    // card is a teaser rather than the whole post, so a client without it
    // renders a gated post and a short public one identically. Covers BOTH gate
    // flows — a tier-gated post and a sold one (whose tier is the auto-minted
    // `post-unlock-*`; `monetization.md` § Per-post pay-to-unlock) — and the
    // room arm: a room post this reader sits on the floor of names the room
    // by the reader's own label (`PostSummary::room_label`, the composer's own
    // "Room: ‹label›" string) instead of the reserved tier, and the card's
    // detail-open is its "open"; a reader not in the room sees the reserved
    // tier (`ui/feed.md` § Encryption at rest → *the app half*, the card).
    if let Some(tier) = &post.gated_tier {
        let text = match &post.room_label {
            Some(label) => feed::post::gate_room(label),
            None => tier.clone(),
        };
        out.push(Element::label(ids::GATED_POST_BADGE, text).within(ids::POST_CARD, i));
    }

    // Buyer's price read (gap (2c), `monetization.md` § Per-post pay-to-unlock
    // → the buyer's price read is post-addressed) — resolved lazily by
    // `fire_resolves` off `fauna.subscriptions.post_unlock.get`. `None` covers
    // both "not yet resolved" and "the nest answered no offer" (a failed
    // read included): both leave the priceless teaser, with
    // claim-code redemption (§5) as the fallback purchase path.
    // The price, the external payment link and the buy affordance are the
    // money plane's buyer half (`dynamic-features.md` § Platform-family
    // surface excision → *The price-and-route class*): a store-safe build
    // shows a sold post as an ordinary gated post — the badge and nothing
    // more. The resolve that feeds this is an ungated `fauna.subscriptions.*`
    // read, so the render carries its own gate (the inert-record trap).
    #[cfg(feature = "payments")]
    if let Some(offer) = &post.unlock_offer {
        out.push(
            Element::label(
                ids::GATED_POST_PRICE,
                offer.price_hint.clone().unwrap_or_default(),
            )
            .within(ids::POST_CARD, i),
        );
        if let Some(url) = &offer.payment_url {
            out.push(
                Element::gesture_button(
                    ids::GATED_POST_PAYMENT_LINK,
                    subscriptions::PAYMENT_URL,
                    true,
                    Gesture::Feed(Action::OpenPaymentLink(url.clone())),
                )
                .within(ids::POST_CARD, i),
            );
        }
        out.push(
            Element::gesture_button(
                ids::GATED_POST_BUY_BUTTON,
                feed::post::BUY_BUTTON,
                true,
                Gesture::Feed(Action::BuyUnlockOffer(post.post_id.clone())),
            )
            .within(ids::POST_CARD, i),
        );
    }

    // The tip surface (`monetization.md` § Tips) — resolved lazily by
    // `fire_resolves`, absent until then and on an untipped post.
    #[cfg(feature = "payments")]
    out.extend(tip_elements(post, Some(("post-card", i))));

    // ONE `protocol-badge` per classified source — the fleet-wide contract
    // (linux `build_protocol_badges` -> `Vec<gtk::Label>`, windows' `ItemsControl`
    // of pills, android's `ProtocolBadge` per badge). tui painted a single label
    // holding every glyph space-joined until 2026-07-29, which reads identically
    // on screen but makes the scoped `count("protocol-badge", scope="post-card[i]")`
    // that every app's e2e asserts return 1 for a two-source post.
    // A third-party bridge's badge also carries the label it declared — the
    // roster entry the token resolved to (`ui/feed.md` § Implementation status
    // today, `SourceKind::Bridged`), the same words the conversations row
    // paints beside the glyph. A first-party source is its glyph alone: the
    // glyph IS its name.
    for kind in classify_sources(&post.source, bridges) {
        let text = match &kind {
            SourceKind::Bridged { label, .. } => format!("{} {label}", kind.glyph().emoji()),
            _ => kind.glyph().emoji().to_string(),
        };
        out.push(Element::label(ids::PROTOCOL_BADGE, text).within(ids::POST_CARD, i));
    }

    for tag in &post.tags {
        out.push(Element::label(ids::TAG_CHIP, format!("#{tag}")).within(ids::POST_CARD, i));
    }

    // The embeds are extracted through the **shared** projections and registered
    // as their own elements — the walker leaves them inert, so this is the only
    // place they get painted.
    if let Some(hash) = post.document.first_image_hash() {
        out.push(post_image_element(hash, images).within(ids::POST_CARD, i));
        // Provenance badge for the SAME hash, once `kick_c2pa_fetches`'
        // check resolves positive — the android `checkBlobC2pa` reachable
        // pattern, generalized to every rendered card (`ui/media.md` §
        // C2PA provenance).
        if has_c2pa(c2pa, hash) {
            out.push(
                Element::label(ids::C2PA_BADGE, fauna_i18n::strings::c2pa::BADGE_LABEL)
                    .within(ids::POST_CARD, i),
            );
        }
    } else if let Some(path) = post.document.proxied_post_image() {
        // A bridged post's own picture (render-model.md § D6c), painted in the
        // same slot, keyed by its proxied path. No C2PA badge: the bytes are the
        // proxy's live answer, never a stored blob the check could address.
        out.push(post_image_element(path, images).within(ids::POST_CARD, i));
    }
    if let Some(hash) = post.document.first_video_hash() {
        out.push(video_thumbnail_element(hash).within(ids::POST_CARD, i));
    } else if let Some(path) = post.document.proxied_post_video() {
        // A bridged post's own video (render-model.md § D6c → *Proxied video*), in the
        // same slot, keyed by its proxied path — never byte-loaded.
        out.push(video_thumbnail_element(path).within(ids::POST_CARD, i));
    }
    for element in crate::document::remote_image_elements(&post.document, remote_images) {
        out.push(element.within(ids::POST_CARD, i));
    }
    if let Some(quoted) = post.document.quoted_post() {
        // A legally-taken-down quote: the nest withheld the envelope, so the
        // card paints the shared tombstone IN PLACE OF the (empty) body and
        // omits the verification badge — there was no envelope to verify, and a
        // blank/broken embed would read as a bug (`moderation.md` § Categories &
        // enforcement item 1; linux's `build_quoted_post_card` does the same).
        // No dedicated test id: the tombstone rides the existing `quoted-post`
        // element's text, and a new e2e id would need ui.yaml approval first.
        // A quote of a post that is GONE (`feed.md` § Post deletion) is painted
        // the same way, with the not-found copy: nothing was decoded either.
        out.push(
            Element::label(ids::QUOTED_POST, quoted_post_text(&quoted)).within(ids::POST_CARD, i),
        );
        // The two embed badges describe a DECODED envelope; a withheld or gone
        // post had none to verify, so neither can apply.
        let decoded = quoted.legal_takedown_ref.is_none() && !quoted.not_found;
        // The embed badge keys off the *quoted* post's own verification,
        // folded into `RenderBlock::QuotedPost::verification` —
        // independent of the focal card's badge above.
        if decoded && quoted.verification == VerificationStatus::Failed {
            out.push(
                Element::label(ids::UNVERIFIED_SOURCE_BADGE, feed::UNVERIFIED_SOURCE)
                    .within(ids::QUOTED_POST, 0)
                    .within(ids::POST_CARD, i),
            );
        }
        // Same independence for the D10 origin badge: a quote written by
        // an external app is badged on the EMBED, whatever the focal
        // card's own origin is.
        if decoded && quoted.authoring_origin == AuthoringOriginStatus::Delegated {
            out.push(
                Element::label(ids::DELEGATED_ORIGIN_BADGE, feed::DELEGATED_ORIGIN)
                    .within(ids::QUOTED_POST, 0)
                    .within(ids::POST_CARD, i),
            );
        }
    }
    // One card per Resolved preview, in body order — `link-preview-card` is `indexed: true`
    // (ui.yaml § link_preview_card, ruled 2026-08-13; render-model.md § D4). The four children
    // are scoped WITHIN their own card, so a two-bare-url body's second title is never read off
    // the first card: the ruling's contract is `post-card[i]/link-preview-card[n]`, and the
    // other six apps get that containment for free from their widget trees. Scope matching is
    // descendant-based (`automation.rs::matches`, e2e-conventions.md § convention 1), so a query
    // scoped to `post-card[i]` alone still resolves this card's children, and so does one naming
    // `link-preview-card[n]` alone.
    for (n, preview) in post
        .document
        .resolved_link_previews()
        .into_iter()
        .enumerate()
    {
        out.push(
            Element::label(ids::LINK_PREVIEW_CARD, preview.title.to_string())
                .within(ids::POST_CARD, i),
        );
        out.push(
            Element::label(ids::LINK_PREVIEW_TITLE, preview.title.to_string())
                .within(ids::LINK_PREVIEW_CARD, n)
                .within(ids::POST_CARD, i),
        );
        out.push(
            Element::label(
                ids::LINK_PREVIEW_DESCRIPTION,
                preview.description.to_string(),
            )
            .within(ids::LINK_PREVIEW_CARD, n)
            .within(ids::POST_CARD, i),
        );
        out.push(
            Element::label(
                ids::LINK_PREVIEW_DOMAIN,
                fauna_core::format::url_host(preview.url),
            )
            .within(ids::LINK_PREVIEW_CARD, n)
            .within(ids::POST_CARD, i),
        );
        // Reveal-gated (render-model.md § D4, the D3 twin): the og:image paints
        // only once `reveal_remote_content` flips it, exactly like a body remote
        // image — `has_blocked_remote_images()` below already counts an
        // unrevealed preview toward the shared reveal button.
        if preview.revealed
            && let Some(hash) = preview.image_hash
        {
            out.push(
                og_image_element(hash, images)
                    .within(ids::LINK_PREVIEW_CARD, n)
                    .within(ids::POST_CARD, i),
            );
        }
    }
    if post.document.has_blocked_remote_images() {
        out.push(
            Element::gesture_button(
                ids::LOAD_REMOTE_CONTENT_BUTTON,
                conversations::detail::LOAD_REMOTE_CONTENT,
                true,
                Gesture::Feed(Action::RevealRemoteImages(post.post_id.clone())),
            )
            .within(ids::POST_CARD, i),
        );
    }

    // The interaction bar: icon + count, **count hidden at 0** (ratified
    // 2026-06-27) — an icon-only button until the post has activity. A REPOST
    // ROW renders no bar at all (`feed.md` § Interaction bar → Repost): its
    // own counters are structurally dark — nothing ever displays them — and
    // the original's live bar is one activation away.
    if !is_repost_row {
        for (id, action, count) in [
            ("feed-like-button", "like", post.like_count),
            ("feed-reply-button", "reply", post.reply_count),
            ("feed-repost-button", "repost", post.repost_count),
            ("feed-quote-button", "quote", post.quote_count),
        ] {
            let label = interaction_label(id, count);
            // `feed-reply-button` arms the compose dialog instead of firing
            // `Interact` directly — reply needs a typed body the raw
            // `interact` door discards (`Action::OpenReplyDialog`'s doc).
            let gesture = if id == "feed-reply-button" {
                Gesture::Feed(Action::OpenReplyDialog(post.post_id.clone()))
            } else {
                Gesture::Feed(Action::Interact {
                    post_id: post.post_id.clone(),
                    action: action.to_string(),
                })
            };
            let mut el = Element::gesture_button(id, label, true, gesture);
            // The two TOGGLE buttons' state (the settings-toggle `state` attr
            // convention): `on` = the viewer's own interaction is live, and
            // the next press reverses it. Reply and quote stay stateless —
            // they compose a new post every time.
            if id == "feed-repost-button" {
                el = el.attr(
                    "state",
                    if post.viewer_repost_id.is_some() {
                        "on"
                    } else {
                        "off"
                    },
                );
            }
            if id == "feed-like-button" {
                el = el.attr("state", if post.viewer_liked { "on" } else { "off" });
            }
            out.push(el.within(ids::POST_CARD, i));
        }
    }
    out
}

/// ui.yaml `feed.sub_pages.post_detail`, triggers `click post-card` and
/// `search-result-item` activation.
///
/// Looks the post up through the shared [`FeedSnapshot::find_post`], **not**
/// `snapshot.posts`: a card click always has its post in the loaded timeline,
/// but a search deep link can name one the feed never loaded, which the manager
/// then fetches into the snapshot's deep-link slot
/// ([`fauna_feed::FeedManager::resolve_post`]). Reading the timeline alone is
/// what made such a hit open a blank dialog. A `post_id` neither holds (the
/// post left the snapshot between click and frame, or the fetch found nothing)
/// still degrades to an empty dialog rather than panicking.
///
/// The dialog's contents register **inside** `feed-post-detail-dialog` — the
/// `.within(dialog_id, 0)` scoping the reply and compose dialogs use — so a
/// read scoped to the dialog finds them, as it does on every other app. They
/// used to register top-level, which no scoped query can match
/// (`Registry::matches`): the detail's `post-image` was unreadable under the
/// dialog, so no test could see a restricted post's picture paint where it
/// first opens. An unscoped read still finds them.
fn post_detail_elements(
    snapshot: &FeedSnapshot,
    post_id: &str,
    author_label: &dyn Fn(&PostSummary) -> String,
    images: &ImageCache,
    remote_images: &ImageCache,
    c2pa: &C2paCache,
) -> Vec<Element> {
    std::iter::once(Element::label(ids::FEED_POST_DETAIL_DIALOG, ""))
        .chain(
            post_detail_contents(snapshot, post_id, author_label, images, remote_images, c2pa)
                .into_iter()
                .map(|e| e.within(ids::FEED_POST_DETAIL_DIALOG, 0)),
        )
        .collect()
}

/// What [`post_detail_elements`] paints inside the dialog.
fn post_detail_contents(
    snapshot: &FeedSnapshot,
    post_id: &str,
    author_label: &dyn Fn(&PostSummary) -> String,
    images: &ImageCache,
    remote_images: &ImageCache,
    c2pa: &C2paCache,
) -> Vec<Element> {
    let mut out = Vec::new();
    let Some(post) = snapshot.find_post(post_id) else {
        return out;
    };
    // A legally-taken-down post: the nest withheld the body from every viewer,
    // so the shared tombstone stands IN PLACE OF the (empty) body — never a
    // blank dialog, and never a page error standing in for a body
    // (`moderation.md` § Categories & enforcement item 1). The author is
    // withheld too, so the dialog carries the id-less pair the surface can
    // honestly show: no author line, no tags, no image, no quote — the
    // `quoted-post` embed's posture one level up. No dedicated test id: the
    // tombstone rides the existing `feed-post-detail-body` element's text, and a
    // new e2e id would need ui.yaml approval first (§ UI Consistency A).
    if let Some(reference) = &post.legal_takedown_ref {
        out.push(Element::label(
            ids::FEED_POST_DETAIL_BODY,
            fauna_i18n::strings::moderation::legal_takedown::tombstone(reference),
        ));
        return out;
    }
    out.push(Element::label(
        ids::FEED_POST_DETAIL_AUTHOR,
        author_label(post),
    ));
    out.push(Element::document(
        ids::FEED_POST_DETAIL_BODY,
        post.document.clone(),
    ));
    for tag in &post.tags {
        out.push(Element::label(ids::TAG_CHIP, format!("#{tag}")));
    }
    if let Some(hash) = post.document.first_image_hash() {
        out.push(post_image_element(hash, images));
        if has_c2pa(c2pa, hash) {
            out.push(Element::label(
                ids::C2PA_BADGE,
                fauna_i18n::strings::c2pa::BADGE_LABEL,
            ));
        }
    } else if let Some(path) = post.document.proxied_post_image() {
        out.push(post_image_element(path, images));
    }
    if let Some(hash) = post.document.first_video_hash() {
        out.push(video_thumbnail_element(hash));
    } else if let Some(path) = post.document.proxied_post_video() {
        out.push(video_thumbnail_element(path));
    }
    out.extend(crate::document::remote_image_elements(
        &post.document,
        remote_images,
    ));
    if let Some(quoted) = post.document.quoted_post() {
        out.push(Element::label(ids::QUOTED_POST, quoted_post_text(&quoted)));
    }
    // The same tip surface the list card paints, unscoped here — the detail
    // sub-page has one post, so its ids need no ancestor index (the
    // `feed-post-detail-*` posture). ui.yaml carries the three ids on
    // `feed.sub_pages.post_detail` for exactly this.
    #[cfg(feature = "payments")]
    out.extend(tip_elements(post, None));
    out
}

/// What a `quoted-post` embed says: the quoted body — or, when there is no body to
/// show, why: the shared legal-takedown tombstone (the nest withheld it,
/// `moderation.md` § Categories & enforcement item 1), or that the post is gone
/// (`feed.md` § Post deletion). One reading for the list card and the detail, so
/// neither can paint a blank embed the other explains.
fn quoted_post_text(quoted: &fauna_core::render::QuotedPostEmbed<'_>) -> String {
    if let Some(reference) = quoted.legal_takedown_ref {
        return fauna_i18n::strings::moderation::legal_takedown::tombstone(reference);
    }
    if quoted.not_found {
        return feed::post::POST_NOT_FOUND.to_string();
    }
    quoted.body.to_string()
}

/// An interaction button's text: the icon alone at zero, icon + count otherwise.
fn interaction_label(id: &str, count: i64) -> String {
    let icon = match id {
        "feed-like-button" => "♥",
        "feed-reply-button" => "↩",
        "feed-repost-button" => "⇄",
        _ => "❝",
    };
    if count > 0 {
        format!("{icon} {count}")
    } else {
        icon.to_string()
    }
}

/// ui.yaml `feed.sub_pages.create_feed`.
///
/// `labelers` and `topics` are the factor picker's two DYNAMIC sources beside
/// the always-present `engagement`: each subscribed labeler's `labeler:<hex>`
/// (`content-moderation-and-ranking.md` § Composition — subscribing is what
/// makes a labeler's labels a weightable factor) and each trained topic's
/// `topic:<hex>` (`topic-factors.md` § Authoring surface & picker). **Both are
/// loaded post-auth** (`settings::spawn_labeler_catalog_refresh` /
/// `spawn_trained_topics_refresh`), so composing a feed from either never
/// requires a Settings visit first — linux reaches the same place by firing its
/// own fetch as the form builds, which tui's one-awaited-op-per-nav-edge
/// contract forbids from a render path.
fn create_feed_elements(
    form: &CreateFeedForm,
    topics: &[fauna_client_personalization::TrainedTopicRow],
    labelers: Vec<String>,
) -> Vec<Element> {
    let options = rule_type_options();
    let rule_keys: Vec<String> = options.iter().map(|o| o.value.clone()).collect();
    let selected = options.iter().find(|o| o.value == form.rule_type);
    let rule_label = selected
        .map(|o| o.label.resolve(fauna_i18n::strings::lookup))
        .unwrap_or_default();
    let kind = selected
        .map(|o| o.input_kind)
        .unwrap_or(fauna_client_feed::RuleInputKind::Text);

    // The shared built-ins lead (`engagement`, `trending`), then the
    // subscribed labelers, then each trained factor's stable key.
    //
    // A corrupt (non-16-byte) registry id has no `factor_key` and therefore no
    // addressable model — it is skipped rather than offered as an option that
    // could never compose.
    let mut factor_keys: Vec<String> = fauna_client_feed::builtin_factor_options()
        .into_iter()
        .map(|o| o.value)
        .collect();
    factor_keys.extend(labelers);
    factor_keys.extend(topics.iter().filter_map(|t| t.factor_key.clone()));
    // A built-in paints its localized label. A labeler has no display name
    // anywhere in the system, so its raw factor key IS its display text — the
    // same answer linux gives. A trained topic paints the user's chosen name,
    // which is sealed, so it never reaches the wire.
    let factor_label = fauna_client_feed::builtin_factor_option(&form.factor)
        .map(|o| o.label.resolve(fauna_i18n::strings::lookup))
        .or_else(|| {
            topics
                .iter()
                .find(|t| t.factor_key.as_deref() == Some(form.factor.as_str()))
                .map(|t| t.name.clone())
        })
        .unwrap_or_else(|| form.factor.clone());

    let mut elements = vec![
        Element::input(
            ids::FEED_CREATE_FEED_NAME,
            form.name.clone(),
            Field::Feed(FeedField::CreateFeedName),
        )
        .labelled(feed::create::FEED_NAME),
        // The select's **value** is the variant key (what the driver selects by
        // and what `create_feed` encodes); the human-readable form of the
        // current choice is the paint-only display value.
        Element::select(
            ids::FEED_RULE_TYPE_SELECT,
            form.rule_type.clone(),
            SelectTarget::RuleType,
            rule_keys,
        )
        .display_value(rule_label),
    ];
    // Show only the inputs the selected type's encoder arm actually reads
    // (`RuleInputKind`, linux's `apply_input_kind` shape): the Toggle types
    // (HasMedia/IsReply) ignore `value`, so the value entry has nothing to
    // bind to; every non-Toggle type's `required` flag is discarded by the
    // encoder, so only the Toggle types show the required/excluded toggle.
    if kind != fauna_client_feed::RuleInputKind::Toggle {
        elements.push(
            Element::input(
                ids::FEED_RULE_VALUE_INPUT,
                form.rule_value.clone(),
                Field::Feed(FeedField::RuleValue),
            )
            .labelled(feed::create::RULE_VALUE_PLACEHOLDER),
        );
        // The label rules (LabelBelow/LabelAbove) need a SECOND input: the
        // category above carries the label name, this one the 0–10 confidence.
        // Without it a tui user has to know to hand-type "spam:5" into the
        // value field — the divergence the other six apps don't have
        // (ui.yaml `feed-rule-threshold-input`, approved 2026-08-04).
        if kind == fauna_client_feed::RuleInputKind::TextAndNumber {
            elements.push(
                Element::input(
                    ids::FEED_RULE_THRESHOLD_INPUT,
                    form.rule_threshold.clone(),
                    Field::Feed(FeedField::RuleThreshold),
                )
                .labelled(feed::create::RULE_THRESHOLD),
            );
        }
    } else {
        elements.push(Element::checkbox_gesture(
            ids::FEED_RULE_REQUIRED_TOGGLE,
            // The label flips with the state — `required:false` is a real
            // exclusion on the nest ("must NOT have media"), so a static
            // "Required" would read as the opposite of the rule being built
            // (shared `rule_required_label`, ui.yaml "Required/excluded toggle").
            fauna_client_feed::rule_required_label(form.rule_required)
                .resolve(fauna_i18n::strings::lookup),
            form.rule_required,
            Gesture::Feed(Action::ToggleRuleRequired),
        ));
    }
    elements.extend([
        Element::gesture_button(
            ids::FEED_ADD_RULE_BUTTON,
            feed::create::ADD_RULE,
            fauna_client_feed::can_add_rule(kind, &form.rule_value, &form.rule_threshold),
            Gesture::Feed(Action::AddRule),
        ),
        Element::select(
            ids::FEED_COMBINATION_SELECT,
            form.combination.clone(),
            SelectTarget::Combination,
            vec!["all".to_string(), "any".to_string()],
        ),
        // The select's **value** is the stable factor key — `engagement`,
        // `trending`, a `labeler:<hex>`, or a trained topic's `topic:<hex>` —
        // which is what the driver selects by and what `create_feed` encodes.
        // The built-in's label or the user's chosen topic NAME is the
        // paint-only display value (the sealed name never reaches the wire),
        // the same stable-key-vs-display split `feed-rule-type-select` uses.
        Element::select(
            ids::FEED_FACTOR_SELECT,
            form.factor.clone(),
            SelectTarget::Factor,
            factor_keys,
        )
        .display_value(factor_label),
        Element::input(
            ids::FEED_FACTOR_WEIGHT_INPUT,
            form.factor_weight.clone(),
            Field::Feed(FeedField::FactorWeight),
        )
        .labelled(feed::create::FACTOR_WEIGHT_PLACEHOLDER),
        Element::checkbox_gesture(
            ids::FEED_FACTOR_GLOBAL_TOGGLE,
            feed::create::FACTOR_GLOBAL_TOGGLE,
            form.factor_global,
            Gesture::Feed(Action::ToggleFactorGlobal),
        ),
        Element::gesture_button(
            ids::FEED_ADD_FACTOR_BUTTON,
            feed::create::ADD_FACTOR,
            true,
            Gesture::Feed(Action::AddFactor),
        ),
        Element::gesture_button(
            ids::CREATE_FEED,
            common::CREATE,
            !form.name.is_empty(),
            Gesture::Feed(Action::CreateFeed),
        ),
        Element::gesture_button(
            ids::FEED_CREATE_CANCEL,
            common::CANCEL,
            true,
            Gesture::Feed(Action::CancelCreateFeed),
        ),
    ]);
    elements
}

/// The inline bridge-subscribe form (`feed.md` § Layout & flow region 5) —
/// the `create_feed_elements` twin. The select's **value** is the bridge's
/// stable `id` (what `subscribe_bridge` encodes, mirroring
/// `feed-rule-type-select`'s stable-key-vs-display split); its **display
/// value** is the human-readable name.
fn bridge_form_elements(
    form: &BridgeSubscribeForm,
    available: &[fauna_feed::AvailableBridge],
) -> Vec<Element> {
    let keys: Vec<String> = available.iter().map(|b| b.id.clone()).collect();
    let label = available
        .iter()
        .find(|b| b.id == form.kind)
        .map(|b| b.name.clone())
        .unwrap_or_default();
    vec![
        Element::select(
            ids::BRIDGE_FORM_BRIDGE_SELECT,
            form.kind.clone(),
            SelectTarget::BridgeKind,
            keys,
        )
        .display_value(label),
        Element::input(
            ids::BRIDGE_FORM_URI_INPUT,
            form.uri.clone(),
            Field::Feed(FeedField::BridgeUri),
        )
        .labelled(feed::bridge_form::URI),
        Element::input(
            ids::BRIDGE_FORM_NAME_INPUT,
            form.name.clone(),
            Field::Feed(FeedField::BridgeName),
        )
        .labelled(feed::bridge_form::NAME),
        Element::gesture_button(
            ids::BRIDGE_FORM_SUBSCRIBE_BUTTON,
            feed::list::SUBSCRIBE_BRIDGE,
            !form.kind.is_empty() && !form.uri.is_empty(),
            Gesture::Feed(Action::SubscribeBridge),
        ),
        Element::gesture_button(
            ids::BRIDGE_FORM_CANCEL_BUTTON,
            common::CANCEL,
            true,
            Gesture::Feed(Action::CancelBridgeForm),
        ),
    ]
}

/// The `data.feed.*` half of `GET /app/state` — ui.yaml's `feed.state_fields`,
/// exactly.
///
/// This is **not** optional decoration: the shared harness reads a post's
/// resolved `media_hash` from here and nowhere else (there is no element behind
/// it), so `test_post_with_image`'s 64-hex-char assertion is answered from this
/// serializer.
pub fn state_json(state: &FeedState) -> serde_json::Value {
    let Some(snapshot) = state.snapshot() else {
        return serde_json::json!({ "posts": [] });
    };
    let posts: Vec<serde_json::Value> = snapshot
        .posts
        .iter()
        .map(|p| {
            serde_json::json!({
                "post_id": p.post_id,
                "author": p.author,
                "body": p.document.to_plaintext(),
                "timestamp": p.timestamp,
                "tags": p.tags,
                "has_media": p.has_media,
                "is_reply": p.is_reply,
                "media_hash": p.media_hash,
                "like_count": p.like_count,
                "reply_count": p.reply_count,
                "repost_count": p.repost_count,
                "quote_count": p.quote_count,
                // The repost carrier + per-viewer pair (`feed.md` § Interaction
                // bar → Repost, ratified 2026-08-10). `reposted_post_id` is how
                // the harness tells a repost row from an empty quote until the
                // rule-A-gated `repost-attribution` id lands; `viewer_repost_id`
                // is the toggle's state (and `unrepost`'s argument).
                "reposted_post_id": p.reposted_post_id,
                "viewer_repost_id": p.viewer_repost_id,
                "viewer_liked": p.viewer_liked,
                // Every link preview in the body with its state, in body order
                // (`RenderDocument::link_previews` — render-model.md § D4). A card
                // is absent while a preview is still resolving too, so this is
                // what lets a test wait until a preview has FAILED before it reads
                // "no card" as "stays a plain link".
                "link_previews": p
                    .document
                    .link_previews()
                    .into_iter()
                    .map(|(url, state)| serde_json::json!({ "url": url, "state": state.name() }))
                    .collect::<Vec<_>>(),
                // The harness's `data.feed.posts[]` contract carries the collapse
                // flag so a state-backed reader can drop a muted post the way an
                // element-backed one does (`actions/feed.py::_feed_posts_from_state`).
                // tui is element-backed, so nothing reads this today — it is here
                // because a half-populated shared contract is how the two reads
                // drift apart.
                "is_muted": !state.revealed_muted.contains(&p.post_id)
                    && state
                        .manager
                        .as_ref()
                        .is_some_and(|m| m.is_muted(&p.post_id)),
            })
        })
        .collect();
    serde_json::json!({ "posts": posts })
}

/// Whether the focus ring sits inside the **last** post card and the snapshot
/// says there is more to fetch — the TUI's analogue of the GUI apps'
/// scroll-edge trigger.
///
/// A post card's children carry the scope `post-card[i]`, and the ring only ever
/// stops on children (the interaction bar), so the focused element's ancestor
/// path is what tells us where in the feed we are.
pub fn at_last_card(app: &App) -> bool {
    let Some(snapshot) = app.feed.snapshot() else {
        return false;
    };
    if !snapshot.has_more || snapshot.posts.is_empty() {
        return false;
    }
    let last = snapshot.posts.len() - 1;
    app.focused().is_some_and(|e| {
        e.path
            .first()
            .is_some_and(|(id, i)| id == "post-card" && *i == last)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal a reply under a restricted post earns reads as the shared
    /// string, not the manager's raw text — and no other error is rewritten.
    #[test]
    fn a_restricted_reply_refusal_paints_the_shared_string() {
        assert_eq!(
            refusal_copy(fauna_client_core::post::REFERENCE_REFUSED_RESTRICTED.to_string()),
            fauna_i18n::strings::feed::REFERENCE_RESTRICTED
        );
        assert_eq!(refusal_copy("nest unreachable".into()), "nest unreachable");
    }
    use std::collections::BTreeSet;

    use fauna_feed::FeedSummaryView;
    use fauna_feed::test_support::{
        TestLinkPreviewSpec, TestPostSpec, TestQuotedSpec, feed_snapshot_with_posts,
    };

    use crate::app::tests::{authed_app, test_session};
    use crate::pages::Page;
    use crate::test_support::art;

    /// An authenticated app on the feed page, with `posts` injected into a real
    /// `FeedManager` (the shared `test-helpers` seam — no nest, no network).
    fn feed_app(posts: Vec<TestPostSpec>) -> App {
        feed_app_with(feed_snapshot_with_posts(posts))
    }

    fn feed_app_with(snapshot: FeedSnapshot) -> App {
        let mut app = authed_app();
        let manager = Arc::new(FeedManager::new(test_session().client, [7u8; 32]));
        manager.set_feed_snapshot_for_test(snapshot);
        app.feed = FeedState {
            manager: Some(manager),
            form: CreateFeedForm::fresh(),
            ..Default::default()
        };
        app.page = Page::Feed;
        app
    }

    /// `FeedSnapshot.error`'s own doc comment says "Page-level error →
    /// `error-message`" — but no page arm ever read it
    /// (`App::page_snapshot_error`'s match had no `Page::Feed` arm), so a
    /// background fetch failure (or the test-only `inject_error_for_test`
    /// twin `ConversationsManager::inject_page_error_for_test` mirrors)
    /// painted nowhere. The e2e finding, pinned here at tier_1 too.
    #[test]
    fn a_feed_manager_error_surfaces_on_the_page_error_line() {
        let app = feed_app(vec![post("hello")]);
        assert_eq!(app.error_line_text(), None, "a clean feed starts errorless");

        let manager = app.feed.manager.clone().expect("feed manager present");
        manager.inject_error_for_test(fauna_core::localized::LocalizedText::key_arg(
            "feed.error_load",
            "message",
            "nest rejected fauna.feed.local.posts",
        ));
        let text = app
            .error_line_text()
            .expect("the injected error must surface on error-message");
        assert!(
            !text.contains("feed.error_load"),
            "must resolve through the i18n table, not leak the raw key: {text:?}"
        );
        assert!(
            text.contains("nest rejected fauna.feed.local.posts"),
            "must carry the injected reason: {text:?}"
        );
    }

    fn post(body: &str) -> TestPostSpec {
        TestPostSpec {
            post_id: "aa".repeat(32),
            author: "bb".repeat(32),
            body: body.to_string(),
            ..Default::default()
        }
    }

    /// Draw `app` through the production render path into a real terminal
    /// buffer the e2e pty's size, and hand back the page as it painted.
    fn draw(app: &App) -> crate::ui::PaintedPage {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        let mut page = None;
        terminal
            .draw(|frame| page = Some(crate::ui::render(frame, app).page))
            .unwrap();
        page.expect("drawn")
    }

    /// The honest-dwell chain at tier_1, end to end through the real paths: a
    /// card below the fold is measured OUT of view; the agent's
    /// scroll-into-view moves the focus ring to it (the scroll, in this app);
    /// the next real draw paints it in view — the registry's `in-viewport`
    /// flips — and the engagement-cue capture measures the whole card inside
    /// the painted band. If scroll-into-view went back to a bare registry
    /// lookup, the second draw would be the first and this fails at `in_view`.
    #[test]
    fn scrolling_a_card_into_view_moves_what_the_frame_and_the_cue_capture_see() {
        use fauna_e2e_agent::{ElementKind, ElementReq};

        let posts: Vec<TestPostSpec> = (0..12)
            .map(|i| TestPostSpec {
                post_id: format!("{i:064x}"),
                author: "bb".repeat(32),
                body: format!("post number {i}"),
                ..Default::default()
            })
            .collect();
        let mut app = feed_app(posts);
        let target_id = format!("{:064x}", 11);

        let before = draw(&app);
        let card = before
            .elements
            .iter()
            .position(|e| e.cue.as_ref().is_some_and(|s| s.post_id == target_id))
            .expect("the last post paints a cue-carrying card");
        assert_eq!(before.in_view(card), Some(false), "starts below the fold");

        let mut registry = crate::automation::Registry::default();
        crate::ui::register_frame(&app, &mut registry, &before);
        let reply = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(crate::automation::perform(
                &mut app,
                &registry,
                &ElementReq {
                    kind: ElementKind::ScrollIntoView,
                    id: ids::POST_CARD.to_string(),
                    index: 11,
                    scope: Vec::new(),
                    arg: String::new(),
                },
            ));
        assert_eq!(reply, serde_json::json!({ "found": true }));

        let after = draw(&app);
        assert_eq!(
            after.in_view(card),
            Some(true),
            "the scroll moved the viewport"
        );
        let row = crate::feed::cues::card_rows(&after)
            .into_iter()
            .find(|r| r.post_id == target_id)
            .expect("the capture measures the card");
        let (top, bottom) = after.band();
        assert!(
            row.top >= top as f64 && row.top + row.height <= bottom as f64,
            "the whole card sits inside the painted band [{top}, {bottom}): {row:?}"
        );
    }

    fn ids(app: &App) -> BTreeSet<String> {
        crate::feed::elements(app)
            .into_iter()
            .map(|e| e.id)
            .filter(|id| !id.is_empty()) // chrome
            .collect()
    }

    /// Opening a post's detail routes through the one `OpenPostDetail` op —
    /// which resolves the post (a no-op for one the timeline holds, a fetch for
    /// a deep link) and then unseals a gated body.
    ///
    /// tui shipped with no unseal glue at all, so every gated post's detail
    /// showed the teaser forever — indistinguishable from "not entitled".
    /// `test_sell_post.py` surfaced this 2026-07-29 alongside the missing badge.
    /// The op is unconditional now (a public card click still costs no round
    /// trip — `FeedManager::resolve_post` returns `Loaded` without touching the
    /// wire, pinned in `fauna-feed`'s own
    /// `resolve_post_is_a_noop_for_a_post_the_timeline_already_holds`), because
    /// a **deep-linked** post has no snapshot row to ask "is this gated?" of
    /// until it has been fetched.
    #[test]
    fn opening_a_post_detail_routes_through_the_resolve_then_unseal_op() {
        let mut gated = post("public teaser");
        gated.gated_tier = Some("post-unlock-abc123".into());
        let post_id = gated.post_id.clone();
        let mut app = feed_app(vec![gated]);
        // The bulk-plane double: reaching the sealed bytes is platform glue, so
        // without a content API there is nothing to fetch with.
        app.feed.content = Some(Arc::new(fauna_nest_http::FakeNestContentApi::new()));
        let op = apply_local(&mut app, Action::OpenPostDetail(post_id.clone()));
        assert!(
            matches!(
                &op,
                Some(Op::OpenPostDetail {
                    content: Some(_),
                    ..
                })
            ),
            "a gated detail-open must carry the content plane to unseal with"
        );

        let mut public = feed_app(vec![post("just a post")]);
        let pid = public.feed.snapshot().unwrap().posts[0].post_id.clone();
        assert!(
            matches!(
                apply_local(&mut public, Action::OpenPostDetail(pid)),
                Some(Op::OpenPostDetail { .. })
            ),
            "every detail-open goes through the one door"
        );
    }

    /// The `post_detail` sub-page renders a **deep-linked** post — one the feed
    /// query never loaded, which the manager fetched into the snapshot's
    /// deep-link slot. Reading `snapshot.posts` alone (what this surface did
    /// until 2026-08-10) painted an empty dialog for exactly this case, which is
    /// what activating a search result produces.
    #[test]
    fn post_detail_renders_a_deep_linked_post_outside_the_timeline() {
        let deep_linked = TestPostSpec {
            post_id: "cc".repeat(32),
            author: "dd".repeat(32),
            body: "the deep-linked body".into(),
            ..Default::default()
        }
        .into_summary();
        let post_id = deep_linked.post_id.clone();
        let mut app = feed_app_with(FeedSnapshot {
            // The timeline holds a different post entirely.
            posts: vec![post("in the feed").into_summary()],
            deep_linked_post: Some(deep_linked),
            status: fauna_feed::FeedStatus::Loaded,
            ..Default::default()
        });
        apply_local(&mut app, Action::OpenPostDetail(post_id));

        let rendered = crate::feed::elements(&app);
        let body = rendered
            .iter()
            .find(|e| e.id == "feed-post-detail-body")
            .expect("the deep-linked post's body renders, not a blank dialog");
        assert!(
            format!("{body:?}").contains("the deep-linked body"),
            "expected the fetched post's real body: {body:?}"
        );
        assert!(
            rendered.iter().any(|e| e.id == "feed-post-detail-author"),
            "and its author"
        );
    }

    /// A **legally taken-down** deep-linked post renders the shared tombstone in
    /// the BODY AREA, where the withheld post would have been — not on
    /// `error-message`, and not as a blank dialog (`ui/feed.md` § The read model
    /// → *Opening a post the timeline never loaded*; `moderation.md` §
    /// Categories & enforcement item 1). The `quoted-post` embed and the DM
    /// bubble already paint theirs in place; this is the third surface of the
    /// same concept.
    #[test]
    fn post_detail_renders_a_taken_down_post_s_tombstone_in_the_body_area() {
        let taken_down = fauna_feed::PostSummary::taken_down("cc".repeat(32), "EU-DSA-2024/12345");
        let post_id = taken_down.post_id.clone();
        let mut app = feed_app_with(FeedSnapshot {
            posts: vec![post("in the feed").into_summary()],
            deep_linked_post: Some(taken_down),
            status: fauna_feed::FeedStatus::Loaded,
            ..Default::default()
        });
        apply_local(&mut app, Action::OpenPostDetail(post_id));

        let rendered = crate::feed::elements(&app);
        let body = rendered
            .iter()
            .find(|e| e.id == "feed-post-detail-body")
            .expect("the tombstone renders in the body area, not a blank dialog");
        // The one shared string, with the reference the obligation names.
        assert!(
            body.text.contains("EU-DSA-2024/12345"),
            "expected the shared legal-takedown tombstone: {body:?}"
        );
        // Nothing of the withheld post is invented to fill the surface.
        assert!(
            !rendered.iter().any(|e| e.id == "feed-post-detail-author"),
            "a withheld post has no author to show"
        );
        assert!(!rendered.iter().any(|e| e.id == "tag-chip"), "nor tags");
    }

    /// A gated post's card carries the tier badge; a public one does not.
    ///
    /// Without this badge a gated post and a short public post render
    /// identically — the reader has no way to know the body is a teaser. tui
    /// shipped without it (and `ui-actual-tui.yaml` did not even list it as
    /// missing); `test_sell_post.py` surfaced the gap 2026-07-29.
    #[test]
    fn a_gated_posts_card_carries_the_tier_badge() {
        let mut gated = post("public teaser");
        gated.gated_tier = Some("post-unlock-abc123".into());
        let app = feed_app(vec![gated]);
        assert!(ids(&app).contains("gated-post-badge"));

        let badge = crate::feed::elements(&app)
            .into_iter()
            .find(|e| e.id == "gated-post-badge")
            .expect("painted");
        assert_eq!(badge.text, "post-unlock-abc123", "the badge names the tier");

        let public = feed_app(vec![post("just a post")]);
        assert!(
            !ids(&public).contains("gated-post-badge"),
            "a public post gets no badge"
        );
    }

    /// A room post's card names the room for a member — by the member's own
    /// label, in the composer's "Room: ‹label›" form — and shows the reserved
    /// tier to a reader who is not in the room (`ui/feed.md` § Encryption at
    /// rest → *Room-restricted — the app half*, the card bullet). The member
    /// test is the snapshot's `own_rooms`, derived by the shared manager on
    /// every read; tui only picks which text to paint.
    #[test]
    fn a_room_posts_card_names_the_room_for_a_member_and_the_tier_for_an_outsider() {
        let room_hex = "c7".repeat(32);
        let mut room_post = post("a teaser for the room");
        room_post.gated_tier = Some(fauna_core::subscription::ROOM_POST_TIER.into());
        room_post.gated_room = Some(room_hex.clone());

        let badge_text = |app: &App| {
            crate::feed::elements(app)
                .into_iter()
                .find(|e| e.id == "gated-post-badge")
                .expect("painted")
                .text
        };

        let outsider = feed_app(vec![room_post.clone()]);
        assert_eq!(
            badge_text(&outsider),
            fauna_core::subscription::ROOM_POST_TIER,
            "not in the room: the reserved tier, as today"
        );

        let mut snapshot = fauna_feed::test_support::feed_snapshot_with_posts(vec![room_post]);
        snapshot.own_rooms = vec![fauna_feed::GateRoomOption {
            room: room_hex,
            label: "Book club".into(),
        }];
        let member = feed_app_with(snapshot);
        assert_eq!(
            badge_text(&member),
            feed::post::gate_room("Book club"),
            "in the room: the room, by the member's own label"
        );
    }

    /// A tipped post's card shows the total, the count and the way into the
    /// attribution window (`monetization.md` § Tips).
    #[test]
    #[cfg(feature = "payments")]
    fn a_tipped_posts_card_shows_total_count_and_the_list_affordance() {
        let mut tipped = post("worth a tip");
        tipped.tips = Some(fauna_feed::TipView {
            total_msats: 21_000,
            tip_count: 3,
            senders: vec![],
            has_more: false,
        });
        let app = feed_app(vec![tipped]);
        let ids = ids(&app);
        assert!(ids.contains("post-tip-total"));
        assert!(ids.contains("post-tip-count"));
        assert!(ids.contains("post-tip-list-button"));

        let els = crate::feed::elements(&app);
        let total = els
            .iter()
            .find(|e| e.id == "post-tip-total")
            .expect("painted");
        assert_eq!(total.text, "21 sats", "sats display, msats wire");
        let count = els
            .iter()
            .find(|e| e.id == "post-tip-count")
            .expect("painted");
        assert_eq!(count.text, "3 tips");
    }

    /// An untipped post paints nothing — and neither does one whose tips have
    /// not resolved yet. Both are the same empty surface, which is also what a
    /// failed read and a nest with no tip mechanism produce.
    #[test]
    #[cfg(feature = "payments")]
    fn an_untipped_or_unresolved_post_paints_no_tip_surface() {
        for tips in [None, Some(fauna_feed::TipView::default())] {
            let mut p = post("nobody tipped this");
            p.tips = tips.clone();
            let ids = ids(&feed_app(vec![p]));
            assert!(!ids.contains("post-tip-total"), "tips={tips:?}");
            assert!(!ids.contains("post-tip-count"), "tips={tips:?}");
            assert!(!ids.contains("post-tip-list-button"), "tips={tips:?}");
        }
    }

    /// **The state the two separate ids exist for.** `tip_count` counts every
    /// tip; `total_msats` sums only the ones whose receipt reported an amount.
    /// A post where none did has real tips and no amount — so the count paints
    /// and the total must NOT, because "0 sats" would say nobody paid
    /// (`monetization.md` § Tips — never coerce a missing amount to 0).
    #[test]
    #[cfg(feature = "payments")]
    fn tips_with_no_parseable_amount_paint_the_count_but_no_total() {
        let mut p = post("tipped, amount unknown");
        p.tips = Some(fauna_feed::TipView {
            total_msats: 0,
            tip_count: 3,
            senders: vec![],
            has_more: false,
        });
        let app = feed_app(vec![p]);
        let ids = ids(&app);
        assert!(
            !ids.contains("post-tip-total"),
            "no summable amount ⇒ no amount shown"
        );
        assert!(ids.contains("post-tip-count"), "but the tips are real");
        assert!(ids.contains("post-tip-list-button"));
    }

    /// The attribution window opens on `post-tip-list-button`, and renders an
    /// unresolvable tipper and a missing amount honestly rather than hiding
    /// either — an outside tip still counts and still displays.
    #[test]
    #[cfg(feature = "payments")]
    fn the_tip_list_opens_and_attributes_every_row_it_was_given() {
        let mut p = post("tipped");
        p.tips = Some(fauna_feed::TipView {
            total_msats: 21_000,
            tip_count: 2,
            senders: vec![
                fauna_feed::TipSenderView {
                    sender: Some("ab".repeat(32)),
                    sender_ref: None,
                    amount_msats: Some(21_000),
                    mechanism: "nostr_zap".into(),
                    received_at: 1_700_000_000,
                },
                fauna_feed::TipSenderView {
                    sender: None,
                    sender_ref: None,
                    amount_msats: None,
                    mechanism: "nostr_zap".into(),
                    received_at: 1_699_999_000,
                },
            ],
            has_more: false,
        });
        let mut app = feed_app(vec![p]);
        assert!(
            !ids(&app).contains("post-tip-list"),
            "closed until asked for"
        );

        let post_id = crate::feed::elements(&app)
            .into_iter()
            .find(|e| e.id == "post-tip-list-button")
            .and_then(|e| match e.role {
                crate::element::Role::Button(Gesture::Feed(Action::OpenTipList(id))) => Some(id),
                _ => None,
            })
            .expect("the button carries its post id");
        assert!(apply_local(&mut app, Action::OpenTipList(post_id)).is_none());

        let els = crate::feed::elements(&app);
        assert!(els.iter().any(|e| e.id == "post-tip-list"));
        let rows: Vec<_> = els.iter().filter(|e| e.id == "post-tip-item").collect();
        assert_eq!(rows.len(), 2, "one row per tip the nest sent, unfiltered");
        assert!(rows[0].text.contains("21 sats"));
        assert!(
            rows[1].text.contains("Someone"),
            "an unresolvable tipper is named, not dropped: {}",
            rows[1].text
        );
        assert!(
            rows[1].text.contains("Amount not reported"),
            "a missing amount says so — never '0 sats': {}",
            rows[1].text
        );
    }

    /// A bounded window says how many it is NOT showing, from the nest's own
    /// `has_more` — never inferred by comparing the row count to a hard-coded
    /// cap.
    #[test]
    #[cfg(feature = "payments")]
    fn a_bounded_tip_list_reports_the_remainder() {
        let mut p = post("very tipped");
        p.tips = Some(fauna_feed::TipView {
            total_msats: 100_000,
            tip_count: 25,
            senders: vec![fauna_feed::TipSenderView {
                sender: Some("cd".repeat(32)),
                sender_ref: None,
                amount_msats: Some(1_000),
                mechanism: "nostr_zap".into(),
                received_at: 1,
            }],
            has_more: true,
        });
        let mut app = feed_app(vec![p]);
        apply_local(&mut app, Action::OpenTipList(post("very tipped").post_id));
        let list = crate::feed::elements(&app)
            .into_iter()
            .find(|e| e.id == "post-tip-list")
            .expect("open");
        assert!(
            list.text.contains("24"),
            "25 tips, 1 row shown ⇒ 24 more: {}",
            list.text
        );
    }

    /// A sold post's card shows the buyer's price + buy affordance once the
    /// offer resolves (gap (2c), `monetization.md` § Per-post pay-to-unlock →
    /// the buyer's price read is post-addressed) — the self-serve teaser
    /// purchase, no claim code needed.
    #[cfg(feature = "payments")]
    #[test]
    fn a_sold_posts_card_shows_price_and_buy_button_once_the_offer_resolves() {
        let mut sold = post("teaser");
        sold.gated_tier = Some("post-unlock-abc123".into());
        sold.unlock_offer = Some(fauna_feed::UnlockOfferView {
            tier_name: "post-unlock-abc123".into(),
            price_hint: Some("$3".into()),
            payment_url: None,
        });
        let app = feed_app(vec![sold]);
        let ids = ids(&app);
        assert!(ids.contains("gated-post-price"));
        assert!(ids.contains("gated-post-buy-button"));
        assert!(
            !ids.contains("gated-post-payment-link"),
            "no payment_url on this offer"
        );

        let price = crate::feed::elements(&app)
            .into_iter()
            .find(|e| e.id == "gated-post-price")
            .expect("painted");
        assert_eq!(price.text, "$3");
    }

    /// The buy affordance stays absent while the offer hasn't resolved yet —
    /// the same render as an ordinary gated post, until
    /// `resolve_post_unlock_offer` fills it in.
    #[test]
    fn a_sold_posts_card_shows_no_buy_affordance_before_the_offer_resolves() {
        let mut sold = post("teaser");
        sold.gated_tier = Some("post-unlock-abc123".into());
        let app = feed_app(vec![sold]);
        let ids = ids(&app);
        assert!(!ids.contains("gated-post-price"));
        assert!(!ids.contains("gated-post-buy-button"));
        assert!(!ids.contains("gated-post-payment-link"));
    }

    /// A `payment_url` on the resolved offer renders the payment-link
    /// affordance too.
    #[cfg(feature = "payments")]
    #[test]
    fn a_sold_posts_card_shows_a_payment_link_when_the_offer_carries_one() {
        let mut sold = post("teaser");
        sold.gated_tier = Some("post-unlock-abc123".into());
        sold.unlock_offer = Some(fauna_feed::UnlockOfferView {
            tier_name: "post-unlock-abc123".into(),
            price_hint: Some("$3".into()),
            payment_url: Some("https://example.com/pay".into()),
        });
        let app = feed_app(vec![sold]);
        assert!(ids(&app).contains("gated-post-payment-link"));
    }

    /// The options `compose-gate-tier-select` offers, in paint order.
    fn gate_options(app: &App) -> Vec<String> {
        crate::feed::elements(app)
            .into_iter()
            .find(|e| e.id == "compose-gate-tier-select")
            .and_then(|e| match e.role {
                crate::element::Role::Select { options, .. } => Some(options),
                _ => None,
            })
            .expect("the gate select is always painted")
    }

    /// The gate select offers Public, then the author's own tiers, then the
    /// sell option — the one control answering "who can read this?".
    #[cfg(feature = "payments")]
    #[test]
    fn the_gate_select_offers_public_the_own_tiers_and_sell() {
        let app = feed_app(vec![post("hello")]);
        assert_eq!(
            gate_options(&app),
            vec![
                feed::post::GATE_PUBLIC.to_string(),
                feed::post::GATE_SELL.to_string(),
            ],
            "with no authored tiers it is Public + sell"
        );
    }

    /// Selecting a tier reveals the teaser and **nothing else** — the sell
    /// controls belong to a different answer.
    #[test]
    fn selecting_a_tier_reveals_only_the_teaser() {
        let mut app = feed_app(vec![post("hello")]);
        assert!(apply_local(&mut app, Action::SetGateTier("supporters".into())).is_none());

        let ids = ids(&app);
        assert!(ids.contains("compose-gate-preview-field"));
        assert!(!ids.contains("compose-sell-price"));
        assert!(!ids.contains("compose-sell-subscribers-free"));
    }

    /// Selecting "Sell this post…" reveals the teaser **and** both sell
    /// controls, with the rank knob defaulting on (user-ratified 2026-07-29).
    #[cfg(feature = "payments")]
    #[test]
    fn selecting_sell_reveals_the_price_and_the_rank_knob_checked() {
        let mut app = feed_app(vec![post("hello")]);
        assert!(apply_local(&mut app, Action::SetGateTier(feed::post::GATE_SELL.into())).is_none());

        let ids = ids(&app);
        assert!(
            ids.contains("compose-gate-preview-field"),
            "a sold post is gated, so it needs a teaser"
        );
        assert!(ids.contains("compose-sell-price"));
        assert!(ids.contains("compose-sell-asking-price"));
        assert!(ids.contains("compose-sell-subscribers-free"));

        let checked = crate::feed::elements(&app)
            .into_iter()
            .find(|e| e.id == "compose-sell-subscribers-free")
            .and_then(|e| match e.role {
                crate::element::Role::Checkbox { checked, .. } => Some(checked),
                _ => None,
            });
        assert_eq!(checked, Some(true), "subscribers get it free by default");
    }

    /// `compose-sell-asking-price` is independent of `compose-sell-price` — the
    /// row's own non-obvious rule (`monetization.md` § The asking price): no
    /// parsing infers one field from the other, and each holds its own buffer.
    #[cfg(feature = "payments")]
    #[test]
    fn the_asking_price_field_is_independent_of_the_price_hint_field() {
        let mut app = feed_app(vec![post("hello")]);
        apply_local(&mut app, Action::SetGateTier(feed::post::GATE_SELL.into()));

        set_field(&mut app.feed, FeedField::ComposeSellPrice, "$5".into());
        set_field(
            &mut app.feed,
            FeedField::ComposeSellAskingPrice,
            "500".into(),
        );

        assert_eq!(field(&app.feed, &FeedField::ComposeSellPrice), "$5");
        assert_eq!(field(&app.feed, &FeedField::ComposeSellAskingPrice), "500");

        let sell = app.feed.snapshot().unwrap().compose.sell.expect("selling");
        assert_eq!(sell.price, "$5");
        assert_eq!(sell.asking_price, "500");
    }

    /// The rank knob is a real toggle, not a painted constant.
    #[cfg(feature = "payments")]
    #[test]
    fn the_rank_knob_toggles() {
        let mut app = feed_app(vec![post("hello")]);
        apply_local(&mut app, Action::SetGateTier(feed::post::GATE_SELL.into()));
        assert!(apply_local(&mut app, Action::ToggleSellSubscribersFree).is_none());

        let sell = app
            .feed
            .snapshot()
            .unwrap()
            .compose
            .sell
            .expect("still selling");
        assert!(!sell.subscribers_get_it_free, "the toggle flipped it off");
    }

    fn text_of(app: &App, id: &str) -> Option<String> {
        crate::feed::elements(app)
            .into_iter()
            .find(|e| e.id == id)
            .map(|e| e.text)
    }

    /// A real file on disk for a `compose-file` path to name.
    fn picked_file(dir: &tempfile::TempDir, name: &str, len: usize) -> String {
        let path = dir.path().join(name);
        std::fs::write(&path, vec![0u8; len]).expect("write the picked file");
        path.to_string_lossy().into_owned()
    }

    fn hash_less(name: &str, size: u64) -> AttachedFile {
        AttachedFile {
            name: name.into(),
            size,
            blob_hash: None,
            media_type: None,
        }
    }

    /// A path typed into `compose-file` that names a real file stages its
    /// hash-less handle on the manager at once, so a draft saved before the
    /// submit carries the file by name (`feed.md` § Persistence → *Attachments
    /// by content address*). tui used to keep the path local until the submit,
    /// so a relaunched draft lost the file with no word to the author — neither
    /// the bar nor the shared refusal ever saw it.
    #[test]
    fn attaching_a_real_path_stages_its_hash_less_handle_on_the_manager() {
        let mut app = feed_app(vec![post("hello")]);
        let dir = tempfile::tempdir().unwrap();
        let path = picked_file(&dir, "photo.png", 2048);

        set_field(&mut app.feed, FeedField::ComposeFile, path.clone());

        assert_eq!(
            field(&app.feed, &FeedField::ComposeFile),
            path,
            "the path field keeps the typed path"
        );
        assert_eq!(
            app.feed.snapshot().unwrap().compose.attached_file,
            Some(hash_less("photo.png", 2048)),
            "the pick must reach the manager as a hash-less handle before any submit"
        );
    }

    /// A restored draft's attachment — a handle with no local path behind it —
    /// is named on the bar with its size, beside a remove control. An edit of
    /// the empty path field cannot drop what that field does not show; the
    /// remove drops the handle and keeps the text.
    #[test]
    fn a_restored_attachment_is_named_on_the_bar_and_only_its_remove_drops_it() {
        let mut app = feed_app(vec![post("hello")]);
        let manager = app.feed.manager.clone().expect("feed manager present");
        manager.update_compose(
            "unfinished".into(),
            String::new(),
            Some(hash_less("photo.png", 2048)),
        );

        let ready = text_of(&app, ids::COMPOSE_FILE_READY)
            .expect("a restored handle must paint compose-file-ready");
        assert!(
            ready.contains("photo.png"),
            "the bar must name the file the refusal will name: {ready:?}"
        );
        assert!(
            ready.contains("KB"),
            "…and its size, on the shared byte scale: {ready:?}"
        );
        assert!(
            ids(&app).contains(ids::COMPOSE_FILE_REMOVE),
            "a restored handle must be removable without attaching another file"
        );

        // The Backspace a human presses on the empty path field.
        set_field(&mut app.feed, FeedField::ComposeFile, String::new());
        assert!(
            manager.snapshot().compose.attached_file.is_some(),
            "an edit of the empty path field must not drop a handle it does not show"
        );

        assert!(apply_local(&mut app, Action::RemoveComposeAttachment).is_none());
        let compose = manager.snapshot().compose;
        assert_eq!(
            compose.attached_file, None,
            "compose-file-remove drops the handle"
        );
        assert_eq!(compose.text, "unfinished", "…and keeps the text");
        let painted = ids(&app);
        assert!(!painted.contains(ids::COMPOSE_FILE_READY));
        assert!(!painted.contains(ids::COMPOSE_FILE_REMOVE));
    }

    /// While the field holds a path, the handle tracks it: a path edited away
    /// from its file drops the handle — and with it the chip, so nothing reads
    /// "ready" for a file that is not there — and clearing the field un-picks it.
    #[test]
    fn a_picked_path_edited_away_from_its_file_drops_its_handle() {
        let mut app = feed_app(vec![post("hello")]);
        let dir = tempfile::tempdir().unwrap();
        let path = picked_file(&dir, "photo.png", 2048);
        set_field(&mut app.feed, FeedField::ComposeFile, path.clone());
        assert!(text_of(&app, ids::COMPOSE_FILE_READY).is_some());

        set_field(&mut app.feed, FeedField::ComposeFile, format!("{path}x"));
        assert_eq!(
            app.feed.snapshot().unwrap().compose.attached_file,
            None,
            "a path naming no file carries no handle"
        );
        assert_eq!(
            text_of(&app, ids::COMPOSE_FILE_READY),
            None,
            "…and paints no ready chip"
        );

        set_field(&mut app.feed, FeedField::ComposeFile, path);
        assert!(app.feed.snapshot().unwrap().compose.attached_file.is_some());
        set_field(&mut app.feed, FeedField::ComposeFile, String::new());
        assert_eq!(
            app.feed.snapshot().unwrap().compose.attached_file,
            None,
            "clearing the path un-picks the file"
        );
        assert_eq!(app.feed.staged_file, None);
    }

    /// Choosing sell after a tier drops the tier's controls — the answers are
    /// mutually exclusive, and the UI must not keep painting the losing one.
    #[cfg(feature = "payments")]
    #[test]
    fn choosing_sell_after_a_tier_replaces_it() {
        let mut app = feed_app(vec![post("hello")]);
        apply_local(&mut app, Action::SetGateTier("supporters".into()));
        apply_local(&mut app, Action::SetGateTier(feed::post::GATE_SELL.into()));

        let snap = app.feed.snapshot().unwrap();
        assert_eq!(snap.compose.gate_tier, None);
        assert!(snap.compose.sell.is_some());
        assert!(ids(&app).contains("compose-sell-price"));
    }

    /// Back to Public: every gated control disappears.
    #[cfg(feature = "payments")]
    #[test]
    fn choosing_public_clears_every_gated_control() {
        let mut app = feed_app(vec![post("hello")]);
        apply_local(&mut app, Action::SetGateTier(feed::post::GATE_SELL.into()));
        apply_local(
            &mut app,
            Action::SetGateTier(feed::post::GATE_PUBLIC.into()),
        );

        let ids = ids(&app);
        assert!(!ids.contains("compose-gate-preview-field"));
        assert!(!ids.contains("compose-sell-price"));
        assert!(!ids.contains("compose-sell-subscribers-free"));
    }

    /// The teaser routes through whichever mode is selected — writing it must
    /// never silently flip the author's gate choice.
    #[cfg(feature = "payments")]
    #[test]
    fn writing_the_teaser_preserves_the_selected_mode() {
        let mut app = feed_app(vec![post("hello")]);
        apply_local(&mut app, Action::SetGateTier(feed::post::GATE_SELL.into()));
        set_field(
            &mut app.feed,
            FeedField::ComposeGatePreview,
            "buy this".into(),
        );

        let snap = app.feed.snapshot().unwrap();
        assert_eq!(snap.compose.gate_preview, "buy this");
        assert!(snap.compose.sell.is_some(), "still selling");

        apply_local(&mut app, Action::SetGateTier("supporters".into()));
        set_field(&mut app.feed, FeedField::ComposeGatePreview, "peek".into());
        let snap = app.feed.snapshot().unwrap();
        assert_eq!(snap.compose.gate_preview, "peek");
        assert_eq!(snap.compose.gate_tier.as_deref(), Some("supporters"));
    }

    /// A `BTreeSet`, not an ordered `Vec`: the post cards are **data-driven**, so
    /// the id *set* is the stable contract and the count is not (the convention
    /// `provisioning_wizard_pages_register_only_ui_yaml_ids` established).
    ///
    /// Every id here is in `ui.yaml`'s `feed:` block. `compose-button` is
    /// deliberately absent — tui's composer is always inline, which the
    /// `feed-compose-bar` component note sanctions (linux and macOS do the same).
    /// It is an omission, not an invisible shim.
    #[test]
    fn the_feed_page_registers_only_ui_yaml_ids() {
        let app = feed_app(vec![post("hello")]);
        let expected: BTreeSet<String> = [
            "page-heading",
            "feed-view",
            "feed-search-field",
            // The built-in Trending pseudo-entry — always painted, above the
            // user's own feeds (`trending.md` § The Trending feed). Unlike
            // `feed-item` it is not data-driven: there is no feed row to drive it.
            "feed-trending-item",
            "feed-create-feed-button",
            "compose-text-field",
            "compose-tags-field",
            "compose-file",
            // Always painted; its two gated children are conditional on the
            // answer selected, which `the_gate_select_reveals_*` cover.
            "compose-gate-tier-select",
            "post-submit-button",
            // The rich-compose opener. Present here and ABSENT once the dialog
            // is open (the composer relocates into it) — unlike `compose-button`,
            // which this client omits entirely: that one toggles a collapsible
            // composer, meaningless where compose is always visible, whereas the
            // dialog is a second editing surface ui.yaml requires of all 7 apps.
            "compose-dialog-button",
            "post-card",
            "post-author",
            "feed-post-text",
            "feed-like-button",
            "feed-reply-button",
            "feed-repost-button",
            "feed-quote-button",
            // The ⋯ overflow's opener, one per card. Its MENU
            // (`feed-post-actions-menu` + the training verbs) is absent here
            // because nothing has opened it — see the menu's own tests below.
            "feed-post-actions-button",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(ids(&app), expected);
    }

    // ── the post-card ⋯ overflow (topic-factors.md § Authoring surface) ──

    const TOPIC: &str = "topic:aabbccddeeff00112233445566778899";

    /// Opening a card's ⋯ paints the menu and BOTH training verbs when the feed
    /// composes exactly one trained factor (train-in-context).
    #[test]
    fn opening_the_overflow_paints_the_menu_and_both_train_verbs() {
        let mut app = feed_app(vec![post("hello")]);
        app.feed
            .manager
            .as_ref()
            .unwrap()
            .set_trained_factor_for_test(TOPIC, fauna_feed::TopicModel::new());
        let post_id = "aa".repeat(32);
        assert!(apply_local(&mut app, Action::OpenPostActions(post_id)).is_none());
        let ids = ids(&app);
        for id in [
            "feed-post-actions-menu",
            "feed-post-more-like-this",
            "feed-post-less-like-this",
        ] {
            assert!(ids.contains(id), "missing {id:?}; have {ids:?}");
        }
        assert!(
            !ids.contains("feed-post-train-target-sheet"),
            "an in-context factor must train directly, not open the target sheet"
        );
    }

    /// With no single trained factor composed, the verbs would have no
    /// unambiguous target — so the menu shows the target sheet instead, and
    /// paints no verbs at all. ui.yaml's component spec states exactly this.
    #[test]
    fn the_overflow_falls_back_to_the_target_sheet_with_no_in_context_factor() {
        let mut app = feed_app(vec![post("hello")]);
        let post_id = "aa".repeat(32);
        assert!(apply_local(&mut app, Action::OpenPostActions(post_id)).is_none());
        let ids = ids(&app);
        assert!(ids.contains("feed-post-train-target-sheet"), "have {ids:?}");
        assert!(
            !ids.contains("feed-post-more-like-this"),
            "a verb with no target must not paint — it would train the wrong model"
        );
    }

    /// The verb's `state` attr is the post's marker in the SEALED model, so it
    /// renders the same after a restart. tui emits no implicit attrs; omitting
    /// it would read downstream as a permanently-unmarked post.
    #[test]
    fn the_train_verbs_publish_their_marker_state() {
        let mut app = feed_app(vec![post("hello")]);
        let post_id = "aa".repeat(32);
        let mut model = fauna_feed::TopicModel::new();
        // Via `TrainVerb`'s own conversion — the same one `train_post` uses —
        // rather than reaching into the model crate's label enum.
        model.train(
            &post_id,
            "hello",
            fauna_feed::TrainVerb::MoreLikeThis.into(),
        );
        app.feed
            .manager
            .as_ref()
            .unwrap()
            .set_trained_factor_for_test(TOPIC, model);
        assert!(apply_local(&mut app, Action::OpenPostActions(post_id)).is_none());
        let state_of = |id: &str| {
            crate::feed::elements(&app)
                .into_iter()
                .find(|e| e.id == id)
                .and_then(|e| {
                    e.attrs
                        .iter()
                        .find(|(k, _)| k == "state")
                        .map(|(_, v)| v.clone())
                })
                .unwrap_or_default()
        };
        assert_eq!(state_of("feed-post-more-like-this"), "on");
        assert_eq!(state_of("feed-post-less-like-this"), "off");
    }

    // ── the own-post web-publishing verbs (web-content-hosting.md
    //    § Published-post management; ui/feed.md § User actions) ──

    /// This app's own actor, as `authed_app`'s session spells it. A post the
    /// viewer authored carries it in `PostSummary.author`, which is the ONLY
    /// thing separating an own post from a stranger's on the wire.
    fn own() -> String {
        crate::app::tests::test_actor_id()
    }

    /// An own post, optionally published (`web_slug`) and optionally gated.
    fn own_post(slug: Option<&str>, tier: Option<&str>) -> TestPostSpec {
        TestPostSpec {
            author: own(),
            web_slug: slug.map(str::to_string),
            gated_tier: tier.map(str::to_string),
            ..post("mine")
        }
    }

    /// A feed app whose web-origin inputs are HYDRATED — the state the ⋯ menu's
    /// copy affordances resolve their origin from. `serving_domain` empty
    /// reproduces the nest-serves-no-web-content case.
    ///
    /// ⚠ The handle is set deliberately, and it is load-bearing: `site_link`
    /// answers `NoHandle` without one, so a fixture that forgot it would resolve
    /// to "no origin" in EVERY case — and the disabled-copy test would then pass
    /// against a handle-less app rather than against the no-serving-domain case
    /// it names. That is exactly how a vacuous assertion is born; this fixture
    /// exists so both directions are real (`origin_resolves_in_the_hydrated_fixture`
    /// pins the positive one).
    fn feed_app_with_origin(posts: Vec<TestPostSpec>, serving_domain: &str) -> App {
        let mut app = feed_app(posts);
        app.settings
            .set_web_origin_for_test("test-handle", serving_domain);
        app
    }

    /// The fixture above really does resolve an origin when it should — the
    /// guard that keeps every "…disables with no origin" assertion below from
    /// passing for the wrong reason.
    #[test]
    fn origin_resolves_in_the_hydrated_fixture() {
        let app = feed_app_with_origin(vec![own_post(Some("s"), None)], "example.com");
        assert_eq!(
            crate::settings::web::site_link(&app.settings)
                .origin
                .as_deref(),
            Some("https://test-handle.example.com/"),
            "the positive direction must be real, or the negative tests are vacuous"
        );
        let none = feed_app_with_origin(vec![own_post(Some("s"), None)], "");
        assert_eq!(
            crate::settings::web::site_link(&none.settings).origin,
            None,
            "and the negative case must be caused by the missing serving domain"
        );
    }

    fn open_menu(app: &mut App) -> BTreeSet<String> {
        assert!(apply_local(app, Action::OpenPostActions("aa".repeat(32))).is_none());
        ids(app)
    }

    /// An UNPUBLISHED own post offers exactly one web verb: publish. The
    /// takedown and both copy affordances describe a page that does not exist.
    #[test]
    fn an_own_unpublished_post_offers_only_the_publish_verb() {
        let mut app = feed_app_with_origin(vec![own_post(None, None)], "example.com");
        let ids = open_menu(&mut app);
        assert!(ids.contains("feed-post-publish-web-button"), "have {ids:?}");
        for absent in [
            "feed-post-unpublish-web-button",
            "feed-post-copy-web-link-button",
            "feed-post-copy-paywall-link-button",
        ] {
            assert!(
                !ids.contains(absent),
                "{absent} describes a page that does not exist yet; have {ids:?}"
            );
        }
    }

    /// A PUBLISHED own post flips the pair: the takedown and the public-link
    /// copy appear, and publish disappears (it would mint a second slug for a
    /// post that already serves).
    #[test]
    fn an_own_published_post_offers_takedown_and_the_public_link() {
        let mut app = feed_app_with_origin(vec![own_post(Some("my-post"), None)], "example.com");
        let ids = open_menu(&mut app);
        for present in [
            "feed-post-unpublish-web-button",
            "feed-post-copy-web-link-button",
        ] {
            assert!(ids.contains(present), "missing {present}; have {ids:?}");
        }
        assert!(
            !ids.contains("feed-post-publish-web-button"),
            "an already-published post must not offer a second publish; have {ids:?}"
        );
    }

    /// The paywall link is published **AND** gated only — an ungated post has no
    /// paywalled body, so the mint would hand out a token for nothing.
    #[test]
    fn the_paywall_link_verb_is_published_and_gated_only() {
        let mut ungated = feed_app_with_origin(vec![own_post(Some("s"), None)], "example.com");
        assert!(
            !open_menu(&mut ungated).contains("feed-post-copy-paywall-link-button"),
            "an ungated post has no paywalled body to hand out"
        );

        let mut gated = feed_app_with_origin(
            vec![own_post(Some("s"), Some("post-unlock-x"))],
            "example.com",
        );
        assert!(
            open_menu(&mut gated).contains("feed-post-copy-paywall-link-button"),
            "a published+gated post is exactly the comp-link case"
        );
    }

    /// A stranger's post offers none of the four, whatever its publish state.
    /// `publish.set` is authorship-gated nest-side too, but a verb that always
    /// refuses is a verb that should never have painted.
    #[test]
    fn a_strangers_post_offers_no_web_verbs() {
        let mut app = feed_app_with_origin(
            vec![TestPostSpec {
                web_slug: Some("theirs".into()),
                gated_tier: Some("post-unlock-x".into()),
                ..post("not mine")
            }],
            "example.com",
        );
        let ids = open_menu(&mut app);
        for absent in [
            "feed-post-publish-web-button",
            "feed-post-unpublish-web-button",
            "feed-post-copy-web-link-button",
            "feed-post-copy-paywall-link-button",
        ] {
            assert!(!ids.contains(absent), "{absent} on a stranger's post");
        }
    }

    /// **Publishing with no serving origin is legal but unreachable, and the UI
    /// must say so** rather than hand out a dead link
    /// (`web-content-hosting.md` § Published-post management). The copy verbs
    /// paint disabled and carry no `value`; the takedown stays live, since it is
    /// the one thing a user with an unreachable site may well want.
    #[test]
    fn the_copy_verbs_disable_when_the_actor_has_no_serving_origin() {
        let mut app = feed_app_with_origin(vec![own_post(Some("s"), Some("post-unlock-x"))], "");
        assert!(apply_local(&mut app, Action::OpenPostActions("aa".repeat(32))).is_none());
        let el = |id: &str| {
            crate::feed::elements(&app)
                .into_iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("missing {id}"))
        };
        for id in [
            "feed-post-copy-web-link-button",
            "feed-post-copy-paywall-link-button",
        ] {
            let e = el(id);
            assert!(!e.enabled, "{id} must be disabled with no serving origin");
            assert_eq!(
                e.attrs
                    .iter()
                    .find(|(k, _)| k == "value")
                    .map(|(_, v)| v.as_str()),
                Some(""),
                "{id} must advertise no link it cannot serve"
            );
        }
        assert!(
            el("feed-post-unpublish-web-button").enabled,
            "a takedown needs no serving origin"
        );
    }

    /// **The web verbs are independent of the training target.** The menu's
    /// train half returns early when no single trained factor composes — and the
    /// web verbs have nothing to do with training, so an own post must still
    /// offer them on an uncomposed feed. Getting this wrong hides the whole
    /// family behind an unrelated condition.
    #[test]
    fn the_web_verbs_paint_with_no_in_context_trained_factor() {
        let mut app = feed_app_with_origin(vec![own_post(Some("s"), None)], "example.com");
        let ids = open_menu(&mut app);
        assert!(
            ids.contains("feed-post-train-target-sheet"),
            "fixture precondition: no in-context factor, so the sheet shows"
        );
        for present in [
            "feed-post-unpublish-web-button",
            "feed-post-copy-web-link-button",
        ] {
            assert!(
                ids.contains(present),
                "{present} must not hide behind the training target; have {ids:?}"
            );
        }
    }

    // ── Own-post delete (`feed.md` § State & data shape → Post deletion) ────

    /// Delete is offered on an own post and **only** an own post. tui was the
    /// last app without this verb — the other six shipped it 2026-07-16..18 —
    /// so the negative half matters as much as the positive: the ⋯ menu on a
    /// stranger's post is reachable (it carries the training verbs), and a
    /// delete painted there could only ever be refused nest-side.
    #[test]
    fn delete_is_offered_on_an_own_post_and_only_an_own_post() {
        let mut app = feed_app_with_origin(vec![own_post(None, None)], "example.com");
        assert!(
            open_menu(&mut app).contains("feed-post-delete-button"),
            "an own post's ⋯ menu must offer delete"
        );

        // `post()` authors to `bb…`, which is not this session's actor.
        let mut theirs = feed_app(vec![post("not mine")]);
        let ids = open_menu(&mut theirs);
        assert!(
            !ids.contains("feed-post-delete-button"),
            "a stranger's post must offer no delete verb; have {ids:?}"
        );
        assert!(
            ids.contains("feed-post-actions-menu"),
            "fixture precondition: the menu itself opens on a stranger's post, \
             so the assertion above is about the verb and not the menu"
        );
    }

    /// The two-step: the first press ARMS and destroys nothing, the confirm is
    /// what dispatches. Delete is the one destructive verb in this menu and the
    /// only one that keeps a confirm — unpublish deliberately has none.
    #[test]
    fn delete_arms_a_confirm_step_and_only_the_confirm_dispatches() {
        let mut app = feed_app_with_origin(vec![own_post(None, None)], "example.com");
        let post_id = "aa".repeat(32);
        open_menu(&mut app);

        assert!(
            apply_local(&mut app, Action::StartDeletePost(post_id.clone())).is_none(),
            "arming the confirm must not dispatch a delete"
        );
        let armed = ids(&app);
        assert!(
            armed.contains("feed-post-delete-confirm-button"),
            "the armed step paints its confirm; have {armed:?}"
        );
        assert!(
            !armed.contains("feed-post-delete-button"),
            "the armed step REPLACES the verb, so a second press cannot land on \
             the un-armed one; have {armed:?}"
        );

        assert!(
            matches!(
                apply_local(&mut app, Action::ConfirmDeletePost(post_id)),
                Some(Op::DeletePost { .. })
            ),
            "the confirm is what dispatches the shared delete"
        );
        assert!(
            app.feed.actions_open.is_none() && app.feed.delete_confirm.is_none(),
            "both close on dispatch — the card is about to go, so a menu left \
             open would anchor to a post that no longer exists"
        );
    }

    /// Arming, closing and reopening the same post's menu presents the UN-armed
    /// verb again. Without this, a user who armed the step, dismissed the menu
    /// and came back would find a one-press destroy waiting for them.
    #[test]
    fn a_reopened_menu_is_never_still_armed() {
        let mut app = feed_app_with_origin(vec![own_post(None, None)], "example.com");
        let post_id = "aa".repeat(32);
        open_menu(&mut app);
        apply_local(&mut app, Action::StartDeletePost(post_id.clone()));
        assert!(app.feed.delete_confirm.is_some(), "armed");

        // Close (the same `OpenPostActions` toggle every dismiss path uses),
        // then reopen.
        apply_local(&mut app, Action::OpenPostActions(post_id.clone()));
        let reopened = open_menu(&mut app);
        assert!(
            app.feed.delete_confirm.is_none(),
            "a fresh open must not be pre-armed"
        );
        assert!(
            reopened.contains("feed-post-delete-button")
                && !reopened.contains("feed-post-delete-confirm-button"),
            "the reopened menu shows the un-armed verb; have {reopened:?}"
        );
    }

    /// The trap this menu has already sprung twice: the training half returns
    /// early when no single trained factor composes, and a verb added *inside*
    /// it vanishes on any uncomposed feed. Delete sits beside the web verbs,
    /// outside that return — the twin of
    /// `the_web_verbs_paint_with_no_in_context_trained_factor`.
    #[test]
    fn delete_paints_with_no_in_context_trained_factor() {
        let mut app = feed_app_with_origin(vec![own_post(None, None)], "example.com");
        let ids = open_menu(&mut app);
        assert!(
            ids.contains("feed-post-train-target-sheet"),
            "fixture precondition: no in-context factor, so the sheet shows"
        );
        assert!(
            ids.contains("feed-post-delete-button"),
            "delete must not hide behind the training target; have {ids:?}"
        );
    }

    /// Both arms re-resolve ownership against the post they NAME rather than
    /// trusting the paint that produced them — the loaded window re-ranks in
    /// place (the `own_published_post` rule, which the web verbs already
    /// follow). A stale id resolves to nothing and drops the gesture.
    #[test]
    fn both_delete_arms_refuse_a_post_that_is_not_the_actors_own() {
        let mut app = feed_app(vec![post("not mine")]);
        let post_id = "aa".repeat(32);
        assert!(apply_local(&mut app, Action::StartDeletePost(post_id.clone())).is_none());
        assert!(
            app.feed.delete_confirm.is_none(),
            "arming must refuse on a post this actor does not own"
        );
        assert!(
            apply_local(&mut app, Action::ConfirmDeletePost(post_id)).is_none(),
            "and the confirm must refuse independently, not trust the arm"
        );
    }

    /// Copying the public link is purely local — origin and slug are both on
    /// screen already — and it paints back **the exact string it clipboarded**
    /// on the button's `copied` attr. No driver reads the OS clipboard, so that
    /// attr is the only thing that can assert the copied CONTENTS rather than
    /// the mere presence of a button (the devices-page lesson).
    #[test]
    fn copying_the_public_link_clipboards_the_posts_own_page_url() {
        let mut app = feed_app_with_origin(vec![own_post(Some("my-post"), None)], "example.com");
        let post_id = "aa".repeat(32);
        assert!(apply_local(&mut app, Action::OpenPostActions(post_id.clone())).is_none());
        assert!(
            apply_local(&mut app, Action::CopyWebLink(post_id.clone())).is_none(),
            "the public link costs no round trip"
        );
        let copied = app.feed.web_copied.as_ref().expect("nothing was copied");
        assert_eq!(
            copied.url,
            "https://test-handle.example.com/post/my-post.html"
        );
        assert_eq!(copied.post_id, post_id);

        let attr = crate::feed::elements(&app)
            .into_iter()
            .find(|e| e.id == "feed-post-copy-web-link-button")
            .and_then(|e| {
                e.attrs
                    .iter()
                    .find(|(k, _)| k == "copied")
                    .map(|(_, v)| v.clone())
            });
        assert_eq!(
            attr.as_deref(),
            Some("https://test-handle.example.com/post/my-post.html"),
            "the button must publish what it actually copied"
        );
    }

    /// **A verb clicked against a post that is no longer eligible does nothing
    /// and mints nothing.** The loaded window re-ranks in place, so every arm
    /// re-resolves the post it names instead of trusting the frame it painted
    /// on. A stranger's post is the sharpest case: `publish.set` is
    /// authorship-gated nest-side, so a leaked op would be a guaranteed refusal
    /// round trip.
    #[test]
    fn a_web_verb_refuses_a_post_it_no_longer_applies_to() {
        let mut app = feed_app_with_origin(
            vec![TestPostSpec {
                web_slug: Some("theirs".into()),
                ..post("not mine")
            }],
            "example.com",
        );
        let id = "aa".repeat(32);
        for action in [
            Action::PublishWeb(id.clone()),
            Action::UnpublishWeb(id.clone()),
            Action::CopyWebLink(id.clone()),
            Action::CopyPaywallLink(id.clone()),
        ] {
            assert!(
                apply_local(&mut app, action).is_none(),
                "a stranger's post must produce no op"
            );
        }
        assert!(
            app.feed.web_copied.is_none(),
            "nothing may reach the clipboard for a post the viewer does not own"
        );

        // Own, but UNPUBLISHED: the takedown and both copies name a page that
        // does not exist, so each must refuse on its own rather than rely on
        // having been painted absent.
        let mut own = feed_app_with_origin(vec![own_post(None, None)], "example.com");
        for action in [
            Action::UnpublishWeb(id.clone()),
            Action::CopyWebLink(id.clone()),
            Action::CopyPaywallLink(id.clone()),
        ] {
            assert!(
                apply_local(&mut own, action).is_none(),
                "an unpublished post has no page to take down or link to"
            );
        }
        assert!(own.feed.web_copied.is_none());
    }

    /// The paywall mint is the one web verb that needs a round trip, so it must
    /// produce an op — and only for a published **and** gated own post.
    #[test]
    fn the_paywall_link_mints_only_for_a_published_gated_own_post() {
        let mut gated = feed_app_with_origin(
            vec![own_post(Some("s"), Some("post-unlock-x"))],
            "example.com",
        );
        let id = "aa".repeat(32);
        assert!(
            matches!(
                apply_local(&mut gated, Action::CopyPaywallLink(id.clone())),
                Some(Op::WebMintPaywallLink { .. })
            ),
            "a published+gated own post must mint"
        );

        let mut ungated = feed_app_with_origin(vec![own_post(Some("s"), None)], "example.com");
        assert!(
            apply_local(&mut ungated, Action::CopyPaywallLink(id)).is_none(),
            "an ungated post has no paywalled body — minting would hand out a token for nothing"
        );
    }

    /// With no serving origin the copy arms refuse **independently of the
    /// paint**: a dead link on the clipboard is worse than no copy, and the
    /// button's disabled state is not something the action layer may assume.
    #[test]
    fn a_copy_verb_refuses_when_there_is_no_serving_origin() {
        let mut app = feed_app_with_origin(vec![own_post(Some("s"), Some("post-unlock-x"))], "");
        let id = "aa".repeat(32);
        assert!(apply_local(&mut app, Action::CopyWebLink(id.clone())).is_none());
        assert!(apply_local(&mut app, Action::CopyPaywallLink(id)).is_none());
        assert!(
            app.feed.web_copied.is_none(),
            "no origin means no link — nothing may be clipboarded"
        );
    }

    /// The mint's outcome is what carries the link back onto the button. Folding
    /// it must also clear the page error: this is the success path, just one
    /// that returns a value.
    #[test]
    fn folding_a_minted_paywall_link_paints_it_and_clears_the_error() {
        let mut app = feed_app_with_origin(
            vec![own_post(Some("s"), Some("post-unlock-x"))],
            "example.com",
        );
        let post_id = "aa".repeat(32);
        app.errors
            .insert(crate::pages::Page::Feed, "stale failure".into());
        assert!(apply_local(&mut app, Action::OpenPostActions(post_id.clone())).is_none());
        crate::feed::apply_outcome(
            &mut app,
            Outcome::CopiedPaywallLink(CopiedFeedLink {
                post_id: post_id.clone(),
                kind: CopiedKind::Paywall,
                url: "https://test-handle.example.com/post/s.html?token=T".into(),
            }),
        );
        assert!(!app.errors.contains_key(&crate::pages::Page::Feed));
        let attr = crate::feed::elements(&app)
            .into_iter()
            .find(|e| e.id == "feed-post-copy-paywall-link-button")
            .and_then(|e| {
                e.attrs
                    .iter()
                    .find(|(k, _)| k == "copied")
                    .map(|(_, v)| v.clone())
            });
        assert_eq!(
            attr.as_deref(),
            Some("https://test-handle.example.com/post/s.html?token=T"),
            "the minted link must land on the button that minted it"
        );
    }

    /// A copied link belongs to **one post**, not to whichever row now sits
    /// where that post was. The window re-ranks in place, so an index-keyed
    /// `copied` would repaint a stranger's button with the author's link.
    #[test]
    fn a_copied_link_stays_with_its_own_post() {
        let other_id = "cc".repeat(32);
        let mut app = feed_app_with_origin(
            vec![
                own_post(Some("mine"), None),
                TestPostSpec {
                    post_id: other_id.clone(),
                    ..own_post(Some("theirs"), None)
                },
            ],
            "example.com",
        );
        let post_id = "aa".repeat(32);
        assert!(apply_local(&mut app, Action::OpenPostActions(post_id.clone())).is_none());
        assert!(apply_local(&mut app, Action::CopyWebLink(post_id)).is_none());

        // The menu re-opens on a DIFFERENT own published post — same button ids,
        // other post. ⚠ It must be a post the snapshot actually HOLDS: an
        // unknown id paints no menu at all, and "no `copied` attr" would then
        // pass without the keying ever being exercised. That vacuity was real
        // here — an earlier version of this test used an id no fixture post
        // carried, and SURVIVED a mutant that dropped the post-id filter
        // outright. The precondition assert below is what keeps it honest.
        app.feed.actions_open = Some(other_id);
        let els = crate::feed::elements(&app);
        assert!(
            els.iter().any(|e| e.id == "feed-post-copy-web-link-button"),
            "fixture precondition: the other post's own copy verb must paint"
        );
        assert!(
            !els.iter()
                .any(|e| e.attrs.iter().any(|(k, _)| k == "copied")),
            "another post's menu must not inherit this post's copied link"
        );
    }

    /// The opener is scoped under its own card — the shared action clicks it
    /// with `scope="post-card[i]"`, so a flat paint would leave that read
    /// resolving to nothing while the page painted perfectly.
    #[test]
    fn the_overflow_opener_is_scoped_under_its_post_card() {
        let app = feed_app(vec![post("one"), post("two")]);
        let openers: Vec<_> = crate::feed::elements(&app)
            .into_iter()
            .filter(|e| e.id == "feed-post-actions-button")
            .collect();
        assert_eq!(openers.len(), 2);
        for (i, o) in openers.iter().enumerate() {
            assert_eq!(
                o.path.first().map(|(c, idx)| (c.as_str(), *idx)),
                Some(("post-card", i)),
                "opener {i} must be scoped under its own card; path = {:?}",
                o.path
            );
        }
    }

    /// Switching feeds changes the composition and with it the verbs' target,
    /// so an overlay opened against the old feed must not survive.
    #[test]
    fn switching_feeds_closes_an_open_overflow() {
        let mut app = feed_app(vec![post("hello")]);
        let post_id = "aa".repeat(32);
        assert!(apply_local(&mut app, Action::OpenPostActions(post_id)).is_none());
        assert!(app.feed.actions_open.is_some());
        let _ = apply_local(&mut app, Action::SelectFeed("other".to_string()));
        assert!(
            app.feed.actions_open.is_none(),
            "a stale menu must not reappear under a different composition"
        );
    }

    /// `trending.md` § The Trending feed: the built-in Trending pseudo-entry sits
    /// **above** the user's own feeds. It is not a `feed-item` — the virtual feed
    /// has no feed row — so it must register its own id, once, ahead of every
    /// `feed-item` in the element order.
    #[test]
    fn the_trending_pseudo_entry_sits_above_the_users_own_feeds() {
        let mut snapshot = feed_snapshot_with_posts(vec![post("hot")]);
        snapshot.feeds = vec![
            FeedSummaryView {
                feed_id: "cc".repeat(32),
                name: "My first feed".to_string(),
                combination: "all".to_string(),
                scope: "local".to_string(),
                contributor_seeds: Vec::new(),
            },
            FeedSummaryView {
                feed_id: "dd".repeat(32),
                name: "My second feed".to_string(),
                combination: "all".to_string(),
                scope: "local".to_string(),
                contributor_seeds: Vec::new(),
            },
        ];
        let app = feed_app_with(snapshot);

        let selector: Vec<String> = crate::feed::elements(&app)
            .into_iter()
            .map(|e| e.id)
            .filter(|id| id == "feed-trending-item" || id == "feed-item")
            .collect();
        assert_eq!(
            selector,
            vec!["feed-trending-item", "feed-item", "feed-item"],
            "Trending is one pseudo-entry ahead of the user's own feeds"
        );
    }

    /// The Trending row fires the shared `select_trending_feed`, never
    /// `select_feed` — the two are different manager methods, and only the former
    /// sets `trending_selected`. Also proves it closes a stale overflow for the
    /// same reason `SelectFeed` does (Trending is a composition switch too).
    #[test]
    fn selecting_trending_yields_the_trending_op_and_closes_an_open_overflow() {
        let mut app = feed_app(vec![post("hot")]);
        let post_id = "aa".repeat(32);
        assert!(apply_local(&mut app, Action::OpenPostActions(post_id)).is_none());
        assert!(app.feed.actions_open.is_some());

        let op = apply_local(&mut app, Action::SelectTrending);
        assert!(
            matches!(op, Some(Op::SelectTrending { .. })),
            "feed-trending-item must run the Trending op, not SelectFeed"
        );
        assert!(
            app.feed.actions_open.is_none(),
            "a stale menu must not survive the composition switch"
        );
    }

    #[test]
    fn the_create_feed_sub_page_registers_only_ui_yaml_ids() {
        let mut app = feed_app(vec![]);
        assert!(apply_local(&mut app, Action::OpenCreateFeed).is_none());
        // Fresh form = the catalog's first type (HasHashtag, a `Text` kind):
        // the value input shows and the required/excluded toggle does not —
        // only the `Toggle` types read `required` (shared `RuleInputKind`).
        let expected: BTreeSet<String> = [
            "page-heading",
            "feed-view",
            "feed-create-feed-name",
            "feed-rule-type-select",
            "feed-rule-value-input",
            "feed-add-rule-button",
            "feed-combination-select",
            "feed-factor-select",
            "feed-factor-weight-input",
            "feed-factor-global-toggle",
            "feed-add-factor-button",
            "create-feed",
            "feed-create-cancel",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(ids(&app), expected);
    }

    /// The picker's three sources: the shared built-ins
    /// (`fauna_client_feed::builtin_factor_options` — `engagement`, `trending`),
    /// then each SUBSCRIBED labeler's `labeler:<hex>`, then each trained
    /// factor's `topic:<hex>` (`content-moderation-and-ranking.md` §
    /// Composition; `topic-factors.md` § Authoring surface & picker;
    /// `trending.md` § The Trending feed — trending is offered like any bus
    /// factor).
    ///
    /// The labeler arm is the one that was missing: this module's own doc
    /// comment claimed it for weeks while `factor_keys` was built from the
    /// trained topics alone, so a subscriber could not compose the labeler they
    /// had just subscribed to — and the automation agent refuses a `select`
    /// value the frame never painted, so it failed loudly rather than silently,
    /// but only for whoever tried it.
    ///
    /// Unsubscribed rows are excluded deliberately: composing one would write
    /// an entry the nest has no scores for, which is an option that does
    /// nothing.
    #[test]
    fn the_factor_picker_offers_subscribed_labelers_between_engagement_and_the_topics() {
        let mut app = feed_app(vec![]);
        let labeler =
            |tag: &str, subscribed: bool| fauna_labeler_catalog_machine::LabelerCatalogEntry {
                labeler_id: tag.repeat(32),
                version: 1,
                publisher_actor: format!("pub-{tag}"),
                artifact_kind: "text-model".to_string(),
                content_kind: "post".to_string(),
                factor: format!("labeler:{}", tag.repeat(32)),
                wasm_hash: "ab".repeat(36),
                wasm_size: 1024,
                subscribed,
                ..Default::default()
            };
        app.settings.labeler_catalog.snapshot =
            Some(fauna_labeler_catalog_machine::LabelerCatalogSnapshot {
                // The UNSUBSCRIBED row is first, so a bug that offered every
                // catalog row cannot pass by coincidence of ordering.
                entries: vec![labeler("bb", false), labeler("aa", true)],
                inspecting: None,
                error: None,
                loaded: true,
            });
        assert!(apply_local(&mut app, Action::OpenCreateFeed).is_none());

        let select = elements(&app)
            .into_iter()
            .find(|e| e.id == "feed-factor-select")
            .expect("feed-factor-select");
        match &select.role {
            crate::element::Role::Select { options, .. } => {
                assert_eq!(
                    options,
                    &vec![
                        "engagement".to_string(),
                        "trending".to_string(),
                        format!("labeler:{}", "aa".repeat(32)),
                    ],
                    "the built-ins lead, then the subscribed labeler; the unsubscribed one \
                     must not be offered"
                );
            }
            other => panic!("feed-factor-select must be a Select, got {other:?}"),
        }
    }

    /// A built-in factor paints its localized picker label, not its raw key —
    /// the stable-key-vs-display split: the select's value stays `trending`
    /// (what the driver selects and `create_feed` encodes), the user reads
    /// "Trending".
    #[test]
    fn a_builtin_factor_paints_its_label_and_keeps_its_key() {
        let mut app = feed_app(vec![]);
        assert!(apply_local(&mut app, Action::OpenCreateFeed).is_none());
        app.feed.form.factor = "trending".to_string();

        let select = elements(&app)
            .into_iter()
            .find(|e| e.id == "feed-factor-select")
            .expect("feed-factor-select");
        assert_eq!(select.text, "trending", "the value stays the factor key");
        match &select.role {
            crate::element::Role::Select { display, .. } => assert_eq!(
                display.as_deref(),
                Some(feed::create::FACTOR_TRENDING),
                "a built-in paints its localized label"
            ),
            other => panic!("feed-factor-select must be a Select, got {other:?}"),
        }
    }

    /// The rule row shows only the widgets the selected type's encoder arm
    /// reads (shared `RuleInputKind` — linux's `apply_input_kind` shape):
    /// a `Toggle` type (HasMedia) swaps the value input for the
    /// required/excluded toggle; a `Number` type (MinReplies) swaps back.
    #[test]
    fn the_rule_input_widget_switches_on_the_selected_types_kind() {
        let mut app = feed_app(vec![]);
        assert!(apply_local(&mut app, Action::OpenCreateFeed).is_none());

        assert!(apply_local(&mut app, Action::SetRuleType("HasMedia".into())).is_none());
        let toggle_ids = ids(&app);
        assert!(
            !toggle_ids.contains("feed-rule-value-input"),
            "a Toggle type ignores `value` — no value input: {toggle_ids:?}"
        );
        assert!(
            toggle_ids.contains("feed-rule-required-toggle"),
            "a Toggle type reads `required` — the toggle shows: {toggle_ids:?}"
        );

        assert!(apply_local(&mut app, Action::SetRuleType("MinReplies".into())).is_none());
        let number_ids = ids(&app);
        assert!(
            number_ids.contains("feed-rule-value-input"),
            "a Number type reads `value` — the input shows: {number_ids:?}"
        );
        assert!(
            !number_ids.contains("feed-rule-required-toggle"),
            "a Number type discards `required` — no toggle: {number_ids:?}"
        );

        // The label rules need BOTH inputs — the category and the 0–10
        // confidence. tui rendered only the value input until 2026-08-04, which
        // forced a user to know to hand-type "spam:5"; the other six apps
        // have always had the second field (ui.yaml `feed-rule-threshold-input`).
        assert!(apply_local(&mut app, Action::SetRuleType("LabelBelow".into())).is_none());
        let label_ids = ids(&app);
        assert!(
            label_ids.contains("feed-rule-value-input"),
            "a TextAndNumber type reads `value` (the category): {label_ids:?}"
        );
        assert!(
            label_ids.contains("feed-rule-threshold-input"),
            "a TextAndNumber type reads a threshold — the second input shows: {label_ids:?}"
        );
        assert!(
            !label_ids.contains("feed-rule-required-toggle"),
            "a TextAndNumber type discards `required` — no toggle: {label_ids:?}"
        );

        // ...and it is the ONLY kind that shows it: a plain Text type must not
        // offer a threshold the encoder would never read.
        assert!(apply_local(&mut app, Action::SetRuleType("HasHashtag".into())).is_none());
        assert!(
            !ids(&app).contains("feed-rule-threshold-input"),
            "a Text type takes no threshold: {:?}",
            ids(&app)
        );
    }

    /// `feed-add-rule-button` gates on `fauna_client_feed::can_add_rule` —
    /// apple's `FeedCreateForm.canAddRule`, lifted (`feed.md` § Add-rule
    /// gating). tui rendered this button permanently enabled until this fix,
    /// the same gap the other five non-apple apps had.
    #[test]
    fn the_add_rule_button_disables_for_invalid_input() {
        let mut app = feed_app(vec![]);
        assert!(apply_local(&mut app, Action::OpenCreateFeed).is_none());
        let add_rule_enabled = |app: &App| {
            crate::feed::elements(app)
                .into_iter()
                .find(|e| e.id == "feed-add-rule-button")
                .expect("feed-add-rule-button must always render")
                .enabled
        };

        // Fresh form = HasHashtag (Text kind), blank value -> disabled.
        assert!(
            !add_rule_enabled(&app),
            "a blank Text-kind value must disable the add-rule button"
        );
        app.feed.form.rule_value = "rust".to_string();
        assert!(
            add_rule_enabled(&app),
            "a non-empty Text-kind value must enable the add-rule button"
        );

        // Number kind: unparseable -> disabled, an integer -> enabled.
        assert!(apply_local(&mut app, Action::SetRuleType("MinReplies".into())).is_none());
        app.feed.form.rule_value = "not-a-number".to_string();
        assert!(
            !add_rule_enabled(&app),
            "an unparseable Number-kind value must disable the add-rule button"
        );
        app.feed.form.rule_value = "5".to_string();
        assert!(
            add_rule_enabled(&app),
            "a parseable Number-kind value must enable the add-rule button"
        );

        // TextAndNumber kind: needs BOTH a category and a parseable threshold.
        assert!(apply_local(&mut app, Action::SetRuleType("LabelBelow".into())).is_none());
        app.feed.form.rule_value = "".to_string();
        assert!(
            !add_rule_enabled(&app),
            "a blank category must disable the add-rule button even with a valid threshold"
        );
        app.feed.form.rule_value = "spam".to_string();
        app.feed.form.rule_threshold = "not-a-number".to_string();
        assert!(
            !add_rule_enabled(&app),
            "an unparseable threshold must disable the add-rule button even with a category"
        );
        app.feed.form.rule_threshold = "5".to_string();
        assert!(
            add_rule_enabled(&app),
            "a category plus a parseable threshold must enable the add-rule button"
        );

        // Toggle kind: always addable, no input to validate.
        assert!(apply_local(&mut app, Action::SetRuleType("HasMedia".into())).is_none());
        assert!(
            add_rule_enabled(&app),
            "a Toggle kind needs no input — always addable"
        );
    }

    /// The label rules pack their two inputs into ONE wire value as
    /// `"category:threshold"` — the shape `encode_filter_rule` splits on, and
    /// the shape linux/windows/android already send. Every other kind sends the
    /// value alone, with no stray `":5"` suffix.
    #[test]
    fn a_label_rule_packs_category_and_threshold_into_one_value() {
        let mut app = feed_app(vec![]);
        assert!(apply_local(&mut app, Action::OpenCreateFeed).is_none());

        assert!(apply_local(&mut app, Action::SetRuleType("LabelBelow".into())).is_none());
        assert_eq!(
            app.feed.form.rule_threshold,
            fauna_client_feed::DEFAULT_RULE_THRESHOLD,
            "the threshold prefills to the shared midpoint, so the rule is addable immediately"
        );
        app.feed.form.rule_value = "spam".to_string();
        app.feed.form.rule_threshold = "3".to_string();
        assert!(apply_local(&mut app, Action::AddRule).is_none());
        assert_eq!(app.feed.form.rules.len(), 1);
        assert_eq!(app.feed.form.rules[0].rule_type, "LabelBelow");
        assert_eq!(
            app.feed.form.rules[0].value, "spam:3",
            "category and threshold pack into one value"
        );
        // And the row round-trips through the shared encoder it was built for.
        let encoded = fauna_client_feed::encode_filter_rule("LabelBelow", "spam:3", false)
            .expect("the packed value must be what encode_filter_rule accepts");
        assert_eq!(
            encoded,
            fauna_core::scoring::FilterRule::LabelBelow {
                category: "spam".into(),
                max_confidence_permille: 300,
            },
            "a 0-10 threshold of 3 reaches the wire as 300 per-mille"
        );
        assert_eq!(
            app.feed.form.rule_threshold,
            fauna_client_feed::DEFAULT_RULE_THRESHOLD,
            "Add resets the threshold to the prefill, not to empty"
        );

        // A non-label rule keeps its bare value — no packing, no ":5" tail.
        assert!(apply_local(&mut app, Action::SetRuleType("HasHashtag".into())).is_none());
        app.feed.form.rule_value = "rust".to_string();
        assert!(apply_local(&mut app, Action::AddRule).is_none());
        assert_eq!(app.feed.form.rules[1].value, "rust");
    }

    /// **The element list is the registry; the viewport clips paint only.**
    ///
    /// Every post in the snapshot gets a card however few fit the terminal — the
    /// e2e pty is 40×120 and a human's is smaller. Clipping the *list* to the
    /// visible rows would silently cap `count("post-card")` at terminal height,
    /// and every cross-app count assertion with it.
    #[test]
    fn every_snapshot_post_registers_a_card_however_small_the_terminal() {
        let app = feed_app((0..40).map(|i| post(&format!("post {i}"))).collect());
        let cards = crate::feed::elements(&app)
            .iter()
            .filter(|e| e.id == "post-card")
            .count();
        assert_eq!(
            cards, 40,
            "the registry must not be clipped to the viewport"
        );
    }

    /// The registry reads the document's **plaintext** — the same value every
    /// other app's `feed-post-text` reports — while paint walks the block tree.
    /// `test_post_body_renders_markdown` asserts exactly this: `bold` renders,
    /// the literal `**` does not survive.
    #[test]
    fn feed_post_text_reads_the_documents_plaintext_not_its_markdown() {
        let app = feed_app(vec![post("hello **bold** world")]);
        let text = crate::feed::elements(&app)
            .into_iter()
            .find(|e| e.id == "feed-post-text")
            .map(|e| e.text)
            .expect("feed-post-text");
        assert!(text.contains("bold"), "got {text:?}");
        assert!(!text.contains("**"), "markdown must not survive: {text:?}");
    }

    // ── The muted-keyword collapse (content-moderation-and-ranking.md § Q3) ──

    /// A feed app whose manager has both `posts` and a live `MutedKeywords`
    /// sealed scorer — the shared `set_muted_keywords_for_test` seam, which is
    /// what makes the collapse branch reachable without a real `fetch_page`.
    fn muted_feed_app(posts: Vec<TestPostSpec>, words: &[&str]) -> App {
        let app = feed_app(posts);
        app.feed
            .manager
            .as_ref()
            .unwrap()
            .set_muted_keywords_for_test(words.iter().map(|w| w.to_string()).collect());
        app
    }

    fn post_with_id(id: &str, body: &str) -> TestPostSpec {
        TestPostSpec {
            post_id: id.to_string(),
            author: "bb".repeat(32),
            body: body.to_string(),
            ..Default::default()
        }
    }

    /// A matching post collapses behind `feed-post-muted` + its reveal button and
    /// — the load-bearing half — **stops registering `feed-post-text`**, which is
    /// what makes `count("feed-post-text")` (the read behind
    /// `actions/feed.py::post_count`) exclude it. A clean post is untouched.
    #[test]
    fn a_matching_post_collapses_and_a_clean_one_does_not() {
        let app = muted_feed_app(
            vec![
                post_with_id("p-clean", "sourdough starter doubled overnight"),
                post_with_id("p-muted", "the finale twist zzspoiler everyone dies"),
            ],
            &["zzspoiler"],
        );
        assert_eq!(
            crate::feed::elements(&app)
                .iter()
                .filter(|e| e.id == "feed-post-muted")
                .count(),
            1,
            "exactly the matching post collapses"
        );
        assert_eq!(
            crate::feed::elements(&app)
                .iter()
                .filter(|e| e.id == "feed-post-text")
                .count(),
            1,
            "the collapsed post must not register its body"
        );
        assert_eq!(
            crate::feed::elements(&app)
                .iter()
                .filter(|e| e.id == "feed-post-muted-reveal-button")
                .count(),
            1
        );
        // Both cards still register — a collapsed post keeps its focus-ring stop
        // (and with it the bottom-of-feed `load_more` trigger).
        assert_eq!(
            crate::feed::elements(&app)
                .iter()
                .filter(|e| e.id == "post-card")
                .count(),
            2
        );
    }

    /// **Entering the Feed tab must re-run the query**, because the sealed
    /// scorers — the user's muted keywords — load only inside the manager's
    /// `reload`. Without this wiring, editing the muted list in Settings and
    /// walking back to the feed rendered against the pre-edit filters: content
    /// the user had just muted stayed visible, silently. This is a regression
    /// pin on the wiring, not on the render (iOS shipped the same bug).
    #[test]
    fn entering_the_feed_tab_re_runs_the_query_so_sealed_scorers_reload() {
        let mut app = feed_app(vec![post("hello")]);
        app.page = Page::Settings;
        let op = app
            .apply(Page::Feed)
            .expect("the nav edge produces a refresh");
        assert!(
            matches!(op, crate::app::PageOp::Feed(Op::RefreshCurrentFeed { .. })),
            "entering Feed must refresh the current query"
        );
        // Re-entering the tab you are already on ALSO refreshes: that is what
        // `reload()` means to every app's e2e (`actions/*.py`: `reload =
        // navigate`), and what a user re-selecting their current tab expects.
        //
        // CHANGED 2026-08-01 — this line previously asserted the opposite (`no
        // round trip`), which was tui pinning a real divergence: every tui
        // `reload()` was a silent no-op while the other apps refetched, so a
        // driver's green ack meant nothing had happened (convention 11). Only
        // the shell-state resets stay edge-scoped.
        assert!(
            matches!(
                app.apply(Page::Feed),
                Some(crate::app::PageOp::Feed(Op::RefreshCurrentFeed { .. }))
            ),
            "a same-page nav is a reload, not a no-op"
        );
    }

    /// A collapsed card must not leak the body — not through `feed-post-text`
    /// (absent), and not through `post-card`'s own text, which is what the
    /// harness's find-a-post-by-its-text helper reads.
    #[test]
    fn a_collapsed_card_leaks_nothing_through_its_own_text() {
        let app = muted_feed_app(
            vec![post_with_id("p-muted", "the finale twist zzspoiler ends")],
            &["zzspoiler"],
        );
        for el in crate::feed::elements(&app) {
            assert!(
                !el.text.contains("zzspoiler"),
                "element {:?} leaked the muted body: {:?}",
                el.id,
                el.text
            );
        }
    }

    /// The collapse's children hang off `post-card[i]` like every other card
    /// child, so a scoped read resolves against the right card in a mixed feed.
    #[test]
    fn the_collapse_children_are_scoped_to_their_own_card() {
        let app = muted_feed_app(
            vec![
                post_with_id("p-clean", "sourdough starter doubled overnight"),
                post_with_id("p-muted", "the finale twist zzspoiler ends"),
            ],
            &["zzspoiler"],
        );
        let els = crate::feed::elements(&app);
        for id in ["feed-post-muted", "feed-post-muted-reveal-button"] {
            let el = els.iter().find(|e| e.id == id).unwrap();
            assert_eq!(
                el.path,
                vec![("post-card".to_string(), 1)],
                "{id} must scope to card 1, the muted one"
            );
        }
    }

    /// Reveal is session-local and per-post: the body comes back for that post
    /// only, and the sealed scorer is untouched (revealing is not un-muting).
    #[test]
    fn revealing_one_post_restores_its_body_without_unmuting_the_term() {
        let mut app = muted_feed_app(
            vec![post_with_id("p-muted", "the finale twist zzspoiler ends")],
            &["zzspoiler"],
        );
        assert!(apply_local(&mut app, Action::RevealMuted("p-muted".into())).is_none());
        let els = crate::feed::elements(&app);
        assert_eq!(els.iter().filter(|e| e.id == "feed-post-muted").count(), 0);
        assert_eq!(els.iter().filter(|e| e.id == "feed-post-text").count(), 1);
        assert!(
            app.feed.manager.as_ref().unwrap().is_muted("p-muted"),
            "the term stays muted — the reveal un-collapses one instance"
        );
    }

    /// The `data.feed.posts[]` harness contract carries the collapse flag, and it
    /// tracks the reveal — so a state-backed reader drops exactly the posts an
    /// element-backed one does.
    #[test]
    fn state_json_reports_is_muted_and_honours_the_reveal() {
        let mut app = muted_feed_app(
            vec![
                post_with_id("p-clean", "sourdough starter doubled overnight"),
                post_with_id("p-muted", "the finale twist zzspoiler ends"),
            ],
            &["zzspoiler"],
        );
        let flags = |app: &App| -> Vec<bool> {
            state_json(&app.feed)["posts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["is_muted"].as_bool().unwrap())
                .collect()
        };
        assert_eq!(flags(&app), vec![false, true]);
        apply_local(&mut app, Action::RevealMuted("p-muted".into()));
        assert_eq!(
            flags(&app),
            vec![false, false],
            "the reveal clears the flag"
        );
    }

    /// Post-card children carry the **same literal id**, addressed by ancestor
    /// scope — `count("tag-chip", scope="post-card[1]")`. Not `post-card-1` (a
    /// dash-index id), not `post-card[1]` as a literal id string. The three
    /// conventions are not interchangeable, and registering the wrong one leaves
    /// every scoped query in the shared suites resolving to nothing.
    #[test]
    fn post_card_children_are_scoped_by_ancestor_index() {
        let mut a = post("first");
        a.tags = vec!["rust".into(), "svelte".into()];
        let mut b = post("second");
        b.tags = vec!["wasm".into()];
        let app = feed_app(vec![a, b]);

        let elements = crate::feed::elements(&app);
        let chips_of = |card: usize| {
            elements
                .iter()
                .filter(|e| e.id == "tag-chip" && e.path == vec![("post-card".to_string(), card)])
                .count()
        };
        assert_eq!(chips_of(0), 2);
        assert_eq!(chips_of(1), 1);
        // The card itself is top-level, so a scoped query correctly misses it.
        let card = elements.iter().find(|e| e.id == "post-card").unwrap();
        assert!(card.path.is_empty());
    }

    /// Count hidden at 0 (ratified 2026-06-27): an icon-only button until the
    /// post has activity.
    #[test]
    fn an_interaction_count_is_hidden_at_zero() {
        let mut p = post("hi");
        p.like_count = 3;
        let app = feed_app(vec![p]);
        let text = |id: &str| {
            crate::feed::elements(&app)
                .into_iter()
                .find(|e| e.id == id)
                .map(|e| e.text)
                .unwrap()
        };
        assert!(text("feed-like-button").contains('3'), "a count shows");
        assert!(
            !text("feed-reply-button")
                .chars()
                .any(|c| c.is_ascii_digit()),
            "a zero count is hidden, not painted as `0`"
        );
    }

    /// A REPOST ROW says so ON SCREEN, through the `repost-attribution`
    /// element (id user-approved 2026-08-11 — it shipped as un-id'd chrome, so
    /// the only witness was a harness state-read of `reposted_post_id`, which
    /// is not what the user sees). An ordinary post must not carry it: the
    /// element's whole job is telling a repost card apart from an
    /// empty-commentary quote card, which look alike once the embed folds in.
    #[test]
    fn only_a_repost_row_paints_the_repost_attribution() {
        let ordinary = post("an ordinary post");
        let mut reposted = post("");
        reposted.reposted_post_id = Some("cc".repeat(32));
        let app = feed_app(vec![ordinary, reposted]);
        let markers: Vec<_> = crate::feed::elements(&app)
            .into_iter()
            .filter(|e| e.id == "repost-attribution")
            .collect();
        assert_eq!(markers.len(), 1, "exactly the repost row carries it");
        assert_eq!(
            markers[0].path,
            vec![("post-card".to_string(), 1)],
            "scoped under ITS OWN card — a flat paint would leave the scoped \
             read resolving to nothing while the page painted perfectly"
        );
        assert!(
            markers[0]
                .text
                .contains(fauna_i18n::strings::feed::post::REPOSTED_MARKER),
            "the marker carries the i18n reposted text, got {:?}",
            markers[0].text
        );
    }

    /// The like button paints its TOGGLE state off `viewer_liked` (`feed.md`
    /// § Interaction bar → Repost ratifies the carrier), the same `state` attr
    /// convention the repost button uses. Without it a lit ♥ is invisible and
    /// the user cannot tell that the next tap takes the like back.
    #[test]
    fn the_like_button_paints_its_toggle_state_off_viewer_liked() {
        let state_of = |app: &_, id: &str| {
            crate::feed::elements(app)
                .into_iter()
                .find(|e| e.id == id)
                .and_then(|e| {
                    e.attrs
                        .iter()
                        .find(|(k, _)| k == "state")
                        .map(|(_, v)| v.clone())
                })
                .unwrap_or_default()
        };
        let app = feed_app(vec![post("not liked yet")]);
        assert_eq!(state_of(&app, "feed-like-button"), "off");

        let mut p = post("liked by me");
        p.viewer_liked = true;
        let app = feed_app(vec![p]);
        assert_eq!(state_of(&app, "feed-like-button"), "on");
        // Reply and quote compose a new post every time — they are not toggles
        // and must not grow a state the render would have to keep true.
        assert_eq!(state_of(&app, "feed-reply-button"), "");
        assert_eq!(state_of(&app, "feed-quote-button"), "");
    }

    /// `data.feed.posts[]` is ui.yaml's declared `feed.state_fields`, and the
    /// shared harness resolves an uploaded image's `media_hash` from **here** —
    /// there is no element behind it. `test_post_with_image`'s 64-hex assertion
    /// is answered by this serializer, so a missing field is a silent test
    /// failure, not a missing feature.
    #[test]
    fn state_json_carries_the_fields_only_it_can_answer() {
        let mut p = post("with an image");
        p.has_media = true;
        p.tags = vec!["photography".into()];
        // `media_hash` is written by `resolve_media` onto the *resolved*
        // `PostSummary`, so it is set on the snapshot rather than the spec — the
        // same place the production fold puts it.
        let mut snapshot = feed_snapshot_with_posts(vec![p]);
        snapshot.posts[0].media_hash = Some("ab".repeat(32));
        let app = feed_app_with(snapshot);

        let json = crate::feed::state_json(&app.feed);
        let post0 = &json["posts"][0];
        assert_eq!(post0["has_media"], true);
        assert_eq!(post0["media_hash"].as_str().unwrap().len(), 64);
        assert_eq!(post0["tags"][0], "photography");
        assert!(post0["body"].as_str().unwrap().contains("with an image"));
    }

    /// `feed.md` § Errors & edge cases (ids approved 2026-09-25): a loaded feed
    /// with no posts registers `feed-empty-state` with the no-posts copy, a
    /// search that matched nothing registers `feed-no-results` instead — never
    /// both, both off the shared `FeedSnapshot::empty_state`.
    #[test]
    fn the_empty_state_registers_the_variant_the_snapshot_answers() {
        let text_of = |app: &App, id: &str| {
            crate::feed::elements(app)
                .into_iter()
                .find(|e| e.id == id)
                .map(|e| e.text)
        };

        let app = feed_app(vec![]);
        assert_eq!(
            text_of(&app, ids::FEED_EMPTY_STATE).as_deref(),
            Some(feed::list::NO_POSTS)
        );
        assert!(!ids(&app).iter().any(|id| id == ids::FEED_NO_RESULTS));

        let mut searching = feed_snapshot_with_posts(vec![]);
        searching.search_query = Some("needle".into());
        let app = feed_app_with(searching);
        assert!(text_of(&app, ids::FEED_NO_RESULTS).is_some());
        assert!(!ids(&app).iter().any(|id| id == ids::FEED_EMPTY_STATE));
    }

    /// `security.md` § App display of unverified content: the badge shows
    /// **iff** `verification == Failed`, scoped to the failing card only.
    #[test]
    fn unverified_source_badge_shows_only_on_failed_verification() {
        let mut failed = post("from an unverified source");
        failed.verification = VerificationStatus::Failed;
        let verified = post("a verified post");
        let app = feed_app(vec![failed, verified]);

        let elements = crate::feed::elements(&app);
        let badge_on = |card: usize| {
            elements.iter().any(|e| {
                e.id == "unverified-source-badge" && e.path == vec![("post-card".to_string(), card)]
            })
        };
        assert!(badge_on(0), "the Failed post should carry the badge");
        assert!(!badge_on(1), "the default-Unchecked post must not");
    }

    /// The D10 audit surface (`atproto-pds-full.md` § Problem 1 → D10 →
    /// *Audit*, ratified 2026-07-29; `principles.md` § grants are audited from
    /// the user's own app): a user looking at their feed can tell **which posts
    /// an external app wrote as them**. The badge shows **iff** the client
    /// verified the envelope AND it was signed by a delegated authoring sub-key.
    ///
    /// The two negative arms are the load-bearing half. A post the client
    /// verified under the account's *own* identity key is the overwhelmingly
    /// common case and must stay unbadged — a badge on every post says nothing.
    /// And an *unverified* post must stay unbadged too: its `signer_auth` cert
    /// is exactly the thing nothing authenticated, so believing its origin claim
    /// would let a forged wire paint itself as "merely delegated".
    #[test]
    fn delegated_origin_badge_shows_only_on_a_verified_delegated_post() {
        let mut delegated = post("written by an external app");
        delegated.verification = VerificationStatus::Verified;
        delegated.authoring_origin = AuthoringOriginStatus::Delegated;

        let mut direct = post("written by the account itself");
        direct.verification = VerificationStatus::Verified;
        direct.authoring_origin = AuthoringOriginStatus::Direct;

        // The nest-index projection default: nothing was decoded, so nothing is
        // known about origin.
        let unchecked = post("a plain list card");

        let app = feed_app(vec![delegated, direct, unchecked]);
        let elements = crate::feed::elements(&app);
        let badge_on = |card: usize| {
            elements.iter().any(|e| {
                e.id == "delegated-origin-badge" && e.path == vec![("post-card".to_string(), card)]
            })
        };
        assert!(
            badge_on(0),
            "an external-app-authored post must be identifiable as one"
        );
        assert!(
            !badge_on(1),
            "a post the account signed itself must NOT be badged"
        );
        assert!(
            !badge_on(2),
            "an undecoded card knows nothing about origin and must NOT be badged"
        );
    }

    /// The same rule under the quoted-post embed, keyed off the *quoted* post's
    /// own origin — the `unverified-source-badge` scoping precedent (a focal
    /// badge and a quoted-embed badge in one card are addressed separately, so
    /// `post-card[i]` and `post-card[i]/quoted-post` never conflate).
    #[test]
    fn a_delegated_quote_badges_the_embed_not_the_focal_card() {
        let mut quoting = post("quoting an external-app post");
        quoting.quoted = Some(TestQuotedSpec {
            post_id: "e".repeat(64),
            author: "5".repeat(64),
            body: "the quoted body".to_string(),
            verification: VerificationStatus::Verified,
            authoring_origin: AuthoringOriginStatus::Delegated,
            ..Default::default()
        });
        let app = feed_app(vec![quoting]);
        let els = crate::feed::elements(&app);

        let badges: Vec<_> = els
            .iter()
            .filter(|e| e.id == "delegated-origin-badge")
            .collect();
        assert_eq!(
            badges.len(),
            1,
            "exactly one badge — the embed's, not the focal card's"
        );
        assert!(
            badges[0].path.iter().any(|(id, _)| id == "quoted-post"),
            "the badge must be scoped under the quoted-post embed, got {:?}",
            badges[0].path
        );
    }

    /// Slice 2b: the quoted-post embed's badge keys off the *quoted* post's own
    /// A legally-taken-down QUOTE paints the shared tombstone in place of the
    /// (withheld, empty) body and omits the verification badge — there was no
    /// envelope to verify, and a blank embed would read as a bug
    /// (`moderation.md` § Categories & enforcement item 1; linux's
    /// `build_quoted_post_card` does the same).
    #[test]
    fn a_legally_taken_down_quote_paints_the_tombstone_not_a_blank_embed() {
        let mut quoting = post("quoting a taken-down post");
        quoting.quoted = Some(TestQuotedSpec {
            post_id: "e".repeat(64),
            author: "5".repeat(64),
            // The envelope was withheld, so the body arrives EMPTY — the exact
            // shape that would otherwise paint a blank card.
            body: String::new(),
            verification: VerificationStatus::Failed,
            legal_takedown_ref: Some("DMCA-2026-0001".to_string()),
            ..Default::default()
        });
        let app = feed_app(vec![quoting]);
        let els = crate::feed::elements(&app);

        let quoted = els
            .iter()
            .find(|e| e.id == "quoted-post")
            .expect("the quoted embed still renders");
        assert_eq!(
            quoted.text,
            fauna_i18n::strings::moderation::legal_takedown::tombstone("DMCA-2026-0001"),
            "the tombstone replaces the withheld body",
        );
        assert!(
            !els.iter().any(|e| e.id == "unverified-source-badge"),
            "no verification badge on a withheld envelope — there was nothing to verify",
        );
    }

    /// A quote of a post that is GONE (`feed.md` § Post deletion — a reference to
    /// a deleted post dangles by design and renders the not-found state) paints
    /// the not-found copy as the embed's text: never a blank embed, and no
    /// embed badge, since nothing was decoded.
    #[test]
    fn a_quote_of_a_deleted_post_paints_the_not_found_state() {
        let mut quoting = post("quoting a post that was deleted");
        quoting.quoted = Some(TestQuotedSpec {
            post_id: "e".repeat(64),
            body: String::new(),
            verification: VerificationStatus::Failed,
            not_found: true,
            ..Default::default()
        });
        let app = feed_app(vec![quoting]);
        let els = crate::feed::elements(&app);

        let quoted = els
            .iter()
            .find(|e| e.id == "quoted-post")
            .expect("the quoted embed still renders");
        assert_eq!(
            quoted.text,
            fauna_i18n::strings::feed::post::POST_NOT_FOUND,
            "the not-found copy replaces the gone post's body",
        );
        assert!(
            !els.iter().any(|e| e.id == "unverified-source-badge"),
            "no verification badge on a post that is gone — there was nothing to verify",
        );
    }

    /// A classified post paints `content-label-badge` scoped under its own
    /// card, off `PostSummary.labels` via the SAME shared pair the moderation
    /// queue uses — so a post carries an identical badge wherever it renders
    /// (`moderation.md` § Per-row badge data path). Scoped, not flat: feed
    /// post-card children are addressed by `scope="post-card[i]"`.
    #[test]
    fn a_classified_post_paints_the_shared_content_label_badge_scoped_to_its_card() {
        let clean = post("an ordinary post");
        let mut flagged = post("a flagged post");
        flagged.labels = vec![fauna_core::content_category::ContentLabelEntry {
            category: "spam".to_string(),
            confidence_per_mille: 900,
        }];
        let app = feed_app(vec![clean, flagged]);
        let els = crate::feed::elements(&app);

        let badges: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "content-label-badge")
            .collect();
        assert_eq!(badges.len(), 1, "only the classified post carries a badge");
        assert_eq!(
            badges[0].path,
            vec![("post-card".to_string(), 1)],
            "scoped to the FLAGGED card (index 1), not the clean one",
        );
    }

    // ── Content-policy render enforcement (family-safety.md § Content policy) ──

    /// A spam-labeled post used by the content-policy tests below. 900‰ is well
    /// over the shared guardian trigger, so the floor bites.
    fn flagged_post(id: &str, body: &str) -> TestPostSpec {
        let mut p = post_with_id(id, body);
        p.labels = vec![fauna_core::content_category::ContentLabelEntry {
            category: "spam".to_string(),
            confidence_per_mille: 900,
        }];
        p
    }

    /// A guardian `block` floor replaces the body with
    /// `content-policy-blocked-notice` and — the load-bearing half — the post's
    /// text **stops carrying the body**, so a blocked post cannot leak what it
    /// says through `post-card`'s own text (the harness's find-a-post-by-text
    /// read) and `count("feed-post-text")` excludes it, exactly as on the other
    /// six apps.
    #[test]
    fn a_guardian_block_floor_replaces_the_body_and_leaks_nothing() {
        let app = feed_app(vec![flagged_post("p-blocked", "buy zzcheappills now")]);
        app.content_policy
            .set_ward_content_policy(Some(fauna_core::obligation::ContentPolicy {
                spam: fauna_core::obligation::ContentFloor::Block,
                ..Default::default()
            }));
        let els = crate::feed::elements(&app);

        assert_eq!(
            els.iter()
                .filter(|e| e.id == "content-policy-blocked-notice")
                .count(),
            1,
            "the blocked post paints the ui.yaml placeholder"
        );
        assert_eq!(
            els.iter().filter(|e| e.id == "feed-post-text").count(),
            0,
            "a blocked post registers no body element"
        );
        for el in &els {
            assert!(
                !el.text.contains("zzcheappills"),
                "element {:?} leaked the blocked body: {:?}",
                el.id,
                el.text
            );
        }
    }

    /// A post the viewer reported — or whose author they reported — paints the
    /// "You reported this" placeholder and nothing of its body; any other post
    /// renders as usual (`moderation.md` § Corollary).
    #[test]
    fn a_reported_post_paints_the_reporter_placeholder_and_leaks_nothing() {
        let reported = "a1".repeat(32);
        let app = feed_app(vec![
            post_with_id(&reported, "zzreportedbody"),
            post_with_id(&"b2".repeat(32), "an ordinary post"),
        ]);
        app.content_policy
            .set_hidden_content(vec![reported.to_ascii_uppercase().to_ascii_lowercase()]);
        let els = crate::feed::elements(&app);
        let notices: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "content-policy-blocked-notice")
            .collect();
        assert_eq!(notices.len(), 1, "exactly the reported post is withheld");
        assert_eq!(
            notices[0].text,
            fauna_i18n::strings::moderation::report::HIDDEN_PLACEHOLDER
        );
        for el in &els {
            assert!(
                !el.text.contains("zzreportedbody"),
                "{:?} leaked the body",
                el.id
            );
        }
        assert!(
            els.iter().any(|e| e.text.contains("an ordinary post")),
            "an unreported post still paints"
        );
    }

    // ── Region content plane (region-blocking.md § The blocked render) ──────

    const REGION_BLOCKED_ID: &str =
        "b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1";
    const REGION_COLLAPSED_ID: &str =
        "c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2";

    /// A feed app on the list page whose device holds a synthetic region
    /// document: a `list` scorer blocks [`REGION_BLOCKED_ID`], another collapses
    /// [`REGION_COLLAPSED_ID`].
    fn region_feed_app() -> App {
        use fauna_client_region::fixtures as fx;
        use fauna_core::region_policy::ContentVerdict;
        let blocked = fauna_core::hex32::decode(REGION_BLOCKED_ID).unwrap();
        let collapsed = fauna_core::hex32::decode(REGION_COLLAPSED_ID).unwrap();
        let (block_rule, block_scorer) = fx::listed_rule(
            "b",
            &[blocked],
            ContentVerdict::Block,
            "Withheld under section 7.",
        );
        let (collapse_rule, collapse_scorer) = fx::listed_rule(
            "c",
            &[collapsed],
            ContentVerdict::Collapse,
            "Hidden under section 9.",
        );
        let doc = fx::document(
            vec![block_rule, collapse_rule],
            vec![block_scorer, collapse_scorer],
        );
        let mut app = feed_app(vec![
            post_with_id("a0", "an ordinary post"),
            post_with_id(REGION_BLOCKED_ID, "zzwithheld body"),
            post_with_id(REGION_COLLAPSED_ID, "a collapsed body"),
        ]);
        app.page = crate::pages::Page::Feed;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        app.region.plane = fx::plane_holding(&doc, now);
        crate::region::apply_rule_sets(&app);
        app
    }

    fn scoped<'a>(els: &'a [Element], id: &str, card: usize) -> Vec<&'a Element> {
        els.iter()
            .filter(|e| e.id == id && e.path.contains(&("post-card".to_string(), card)))
            .collect()
    }

    /// The region placeholder stands IN PLACE of the exact card's body: the
    /// app's frame naming the region and its authority, the authority's name,
    /// and its reason verbatim — and the body never reaches any element.
    #[test]
    fn a_region_block_paints_the_reasoned_placeholder_on_the_exact_card() {
        use fauna_client_region::fixtures as fx;
        let app = region_feed_app();
        let els = crate::feed::elements(&app);

        let notice = scoped(&els, "region-blocked-notice", 1);
        assert_eq!(
            notice.len(),
            1,
            "the listed post paints the region placeholder"
        );
        assert_eq!(
            notice[0].text,
            fauna_i18n::strings::region::blocked_notice(fx::REGION, fx::AUTHORITY)
        );
        assert_eq!(
            scoped(&els, "region-blocked-authority", 1)[0].text,
            fx::AUTHORITY
        );
        assert_eq!(
            scoped(&els, "region-blocked-reason", 1)[0].text,
            "Withheld under section 7."
        );
        assert!(scoped(&els, "region-collapsed-reveal-button", 1).is_empty());
        assert!(scoped(&els, "feed-post-text", 1).is_empty());
        assert!(
            scoped(&els, "region-blocked-notice", 0).is_empty(),
            "the clean card is untouched"
        );
        for el in &els {
            assert!(
                !el.text.contains("zzwithheld"),
                "{:?} leaked the body",
                el.id
            );
        }
    }

    /// A region `collapse` paints the reveal; revealing shows the body.
    #[test]
    fn a_region_collapse_reveals_on_the_reveal_button() {
        let mut app = region_feed_app();
        let els = crate::feed::elements(&app);
        assert_eq!(scoped(&els, "region-collapsed-reveal-button", 2).len(), 1);
        assert!(scoped(&els, "feed-post-text", 2).is_empty());

        app.feed
            .revealed_content
            .insert(REGION_COLLAPSED_ID.to_string());
        let els = crate::feed::elements(&app);
        assert!(scoped(&els, "region-blocked-notice", 2).is_empty());
        assert_eq!(scoped(&els, "feed-post-text", 2).len(), 1);
    }

    /// Post detail composes the region source too — opening a withheld post
    /// shows the placeholder, never the body.
    #[test]
    fn post_detail_of_a_region_blocked_post_shows_the_placeholder() {
        let mut app = region_feed_app();
        app.feed.mode = Mode::PostDetail(REGION_BLOCKED_ID.to_string());
        let els = crate::feed::elements(&app);
        assert!(els.iter().any(|e| e.id == "region-blocked-notice"));
        for el in &els {
            assert!(
                !el.text.contains("zzwithheld"),
                "{:?} leaked the body",
                el.id
            );
        }
    }

    /// Convention 17's "a region Block never renders silent": the verdict walk
    /// and the painted block placeholders agree on a correct render.
    #[test]
    fn the_region_block_render_counts_agree_on_a_correct_render() {
        let app = region_feed_app();
        assert_eq!(
            crate::region::block_render_json(&app),
            serde_json::json!({"blocked": 1, "placeholders": 1})
        );
    }

    /// With no region document the plane changes nothing (every device today).
    #[test]
    fn no_region_document_changes_nothing() {
        let app = feed_app(vec![post_with_id(REGION_BLOCKED_ID, "zzwithheld body")]);
        let els = crate::feed::elements(&app);
        assert!(!els.iter().any(|e| e.id.starts_with("region-")));
        assert_eq!(els.iter().filter(|e| e.id == "feed-post-text").count(), 1);
    }

    /// **A `block` is absolute — no reveal.** The two revealable collapses each
    /// paint a reveal affordance; the block arm must not, or a ward could tap
    /// past their guardian's floor.
    #[test]
    fn a_blocked_post_offers_no_reveal() {
        let app = feed_app(vec![flagged_post("p-blocked", "buy zzcheappills now")]);
        app.content_policy
            .set_ward_content_policy(Some(fauna_core::obligation::ContentPolicy {
                spam: fauna_core::obligation::ContentFloor::Block,
                ..Default::default()
            }));
        let els = crate::feed::elements(&app);
        assert!(
            !els.iter()
                .any(|e| matches!(e.role, crate::element::Role::Button(_))
                    && e.text == fauna_i18n::strings::family::CONTENT_REVEAL_BUTTON),
            "a block floor must paint no reveal affordance"
        );
    }

    /// **Block beats mute.** A post that is BOTH muted (revealable) and blocked
    /// must render as blocked — otherwise revealing the mute would walk it
    /// straight past the guardian's block. This is why the block arm is ordered
    /// first, matching linux's `post_list.rs` and web's `PostCard.svelte`.
    #[test]
    fn a_post_that_is_both_muted_and_blocked_renders_blocked() {
        let app = muted_feed_app(
            vec![flagged_post("p-both", "the finale twist zzspoiler ends")],
            &["zzspoiler"],
        );
        app.content_policy
            .set_ward_content_policy(Some(fauna_core::obligation::ContentPolicy {
                spam: fauna_core::obligation::ContentFloor::Block,
                ..Default::default()
            }));
        let els = crate::feed::elements(&app);
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "content-policy-blocked-notice")
                .count(),
            1,
            "the block arm wins"
        );
        assert_eq!(
            els.iter().filter(|e| e.id == "feed-post-muted").count(),
            0,
            "and the muted arm — whose reveal would bypass the block — never runs"
        );
    }

    /// The viewer's OWN spam threshold collapses a post with no guardian
    /// anywhere in sight (`family-safety.md` § Content policy — the engine is
    /// built "once, for every user"), and the collapse is revealable.
    #[test]
    fn an_unsupervised_viewers_own_threshold_collapses_and_reveals() {
        let mut app = feed_app(vec![flagged_post("p-spam", "buy zzcheappills now")]);
        app.content_policy
            .set_spam_preferences(Some(&fauna_client_spam::spam::SpamPreferences {
                spam_threshold: 500,
                phishing_threshold: 500,
                extra: Default::default(),
            }));

        let els = crate::feed::elements(&app);
        assert_eq!(
            els.iter().filter(|e| e.id == "feed-post-text").count(),
            0,
            "the collapsed post registers no body"
        );
        for el in &els {
            assert!(
                !el.text.contains("zzcheappills"),
                "element {:?} leaked the collapsed body",
                el.id
            );
        }
        // The reveal is untagged (ui.yaml scopes no id to this arm — linux and
        // web made the same call), so it is found by role + text, and it must be
        // FOCUSABLE: a TUI has no mouse, so an unfocusable control would be a
        // dead affordance rather than merely an un-automatable one.
        let reveal = els
            .iter()
            .find(|e| {
                matches!(e.role, crate::element::Role::Button(_))
                    && e.text == fauna_i18n::strings::family::CONTENT_REVEAL_BUTTON
            })
            .expect("a collapse offers a reveal");
        assert!(
            reveal.focusable(),
            "the reveal must be reachable by keyboard"
        );
        assert!(
            reveal.id.is_empty(),
            "and it must not mint a app-specific id"
        );

        // Revealing restores the body for that post, session-locally.
        assert!(apply_local(&mut app, Action::RevealContent("p-spam".into())).is_none());
        let els = crate::feed::elements(&app);
        assert_eq!(
            els.iter().filter(|e| e.id == "feed-post-text").count(),
            1,
            "the revealed post paints its body again"
        );
    }

    /// **One `protocol-badge` per classified source**, scoped to its own card —
    /// the fleet-wide contract (`ui.yaml` § protocol-badge; linux's
    /// `build_protocol_badges` returns a `Vec`, windows an `ItemsControl` of
    /// pills, android one `ProtocolBadge` per badge). tui painted a SINGLE label
    /// holding every glyph space-joined until 2026-07-29, which looks identical
    /// on screen but makes the scoped `count("protocol-badge", …)` that every
    /// app's e2e asserts read 1 for a two-source post
    /// (`test_feed_protocol_badge.py`).
    #[test]
    fn a_two_source_post_paints_one_protocol_badge_per_classified_source() {
        let mut none = post("no source at all");
        none.source = String::new();
        let mut one = post("bridged from one source");
        one.source = "bluesky".to_string();
        let mut two = post("bridged from two sources");
        two.source = "bluesky,nostr".to_string();
        let app = feed_app(vec![none, one, two]);
        let els = crate::feed::elements(&app);

        let on_card = |i: usize| {
            els.iter()
                .filter(|e| {
                    e.id == "protocol-badge" && e.path == vec![("post-card".to_string(), i)]
                })
                .count()
        };
        assert_eq!(on_card(0), 0, "an empty source field paints no badge");
        assert_eq!(on_card(1), 1, "a single classified source paints one badge");
        assert_eq!(
            on_card(2),
            2,
            "two distinct classified sources paint TWO badges, not one joined label",
        );
    }

    /// verification, independent of the focal card's (both here are the default
    /// `Unchecked`, so neither focal card shows a badge of its own).
    #[test]
    fn quoted_embed_badge_keys_off_the_quoted_posts_own_verification() {
        let mut failing_quote = post("quoting an unverified post");
        // Struct-update, not a hand-listed field set — the project-wide
        // fixture-shape convention, so a branch that grows `TestQuotedSpec`
        // merges cleanly instead of breaking every construction site.
        failing_quote.quoted = Some(TestQuotedSpec {
            post_id: "e".repeat(64),
            author: "5".repeat(64),
            body: "the unverified quoted body".to_string(),
            verification: VerificationStatus::Failed,
            ..Default::default()
        });
        let mut clean_quote = post("quoting a verified post");
        clean_quote.quoted = Some(TestQuotedSpec {
            post_id: "0".repeat(64),
            author: "7".repeat(64),
            body: "the verified quoted body".to_string(),
            verification: VerificationStatus::Verified,
            ..Default::default()
        });
        let app = feed_app(vec![failing_quote, clean_quote]);

        let elements = crate::feed::elements(&app);
        let embed_badge_on = |card: usize| {
            elements.iter().any(|e| {
                e.id == "unverified-source-badge"
                    && e.path
                        == vec![
                            ("post-card".to_string(), card),
                            ("quoted-post".to_string(), 0),
                        ]
            })
        };
        assert!(
            embed_badge_on(0),
            "a Failed quote should carry the embed badge"
        );
        assert!(!embed_badge_on(1), "a Verified quote must not");
    }

    /// render-model.md § D4: the resolved card paints title/description/domain
    /// unconditionally, and the og:image only once the post is revealed — the D3
    /// reveal gate, shared with a body remote image.
    #[test]
    fn link_preview_card_renders_children_and_reveal_gates_the_image() {
        let mut p = post("[https://example.com/article](https://example.com/article)");
        p.link_previews = vec![TestLinkPreviewSpec {
            url: "https://example.com/article".to_string(),
            title: "Example Article Title".to_string(),
            description: "A short description.".to_string(),
            image_hash: Some("ab".repeat(32)),
            revealed: false,
        }];
        let app = feed_app(vec![p]);

        let text = |id: &str| {
            crate::feed::elements(&app)
                .into_iter()
                .find(|e| e.id == id)
                .map(|e| e.text)
        };
        assert_eq!(
            text("link-preview-title").as_deref(),
            Some("Example Article Title")
        );
        assert_eq!(
            text("link-preview-description").as_deref(),
            Some("A short description.")
        );
        assert_eq!(text("link-preview-domain").as_deref(), Some("example.com"));
        assert!(
            text("link-preview-image").is_none(),
            "blocked-by-default until revealed"
        );

        let mut revealed = post("[https://example.com/article](https://example.com/article)");
        revealed.link_previews = vec![TestLinkPreviewSpec {
            url: "https://example.com/article".to_string(),
            title: "Example Article Title".to_string(),
            description: "A short description.".to_string(),
            image_hash: Some("ab".repeat(32)),
            revealed: true,
        }];
        let app = feed_app(vec![revealed]);
        let image = crate::feed::elements(&app)
            .into_iter()
            .find(|e| e.id == "link-preview-image")
            .map(|e| e.text);
        assert_eq!(image.as_deref(), Some("ab".repeat(32)).as_deref());
    }

    /// The `doc-remote-image` promote, end to end on a card: the element is
    /// scoped to its post-card in both states, only a revealed url is offered to
    /// the fetcher, and the folded art is what turns the placeholder into paint.
    ///
    /// This is the whole of what `test_feed_remote_image.py` asserts through the
    /// real binary, pinned here in milliseconds instead of an e2e slot.
    #[test]
    fn a_remote_image_registers_on_its_card_and_paints_after_the_reveal() {
        let url = "https://x.example/c.png";
        let mut app = feed_app(vec![post(&format!("![a cat]({url})"))]);
        let text = |app: &App| {
            crate::feed::elements(app)
                .into_iter()
                .find(|e| e.id == "doc-remote-image")
                .map(|e| (e.text, e.path))
        };

        // Blocked: registered (so an absence assertion about PAINT is not
        // vacuous), scoped to its card, and unpainted. The reveal button is up.
        let (blocked, path) = text(&app).expect("a blocked remote image still registers");
        assert!(!blocked.contains('▀'), "blocked content must not paint");
        assert_eq!(
            path.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(),
            vec!["post-card"],
            "the card's children are addressed by ancestor scope"
        );
        assert!(ids(&app).contains("load-remote-content-button"));
        assert!(
            kick_remote_image_fetches(&mut app).is_none(),
            "nothing is fetched before the reader reveals it"
        );

        // Reveal → the manager re-emits with `revealed: true`, and the tick that
        // follows is the one that first has a url to fetch.
        apply_local(&mut app, Action::RevealRemoteImages("aa".repeat(32)));
        assert!(
            !ids(&app).contains("load-remote-content-button"),
            "the button clears once nothing is blocked"
        );
        match kick_remote_image_fetches(&mut app) {
            Some(Op::FetchRemoteImages { urls }) => assert_eq!(urls, vec![url.to_string()]),
            _ => panic!("the revealed url must be requested"),
        }
        assert!(
            !text(&app).unwrap().0.contains('▀'),
            "still a placeholder while the bytes are in flight"
        );

        // The fold is what paints — and a failure degrades back to the
        // placeholder rather than blanking the row or raising the banner.
        apply_outcome(
            &mut app,
            Outcome::RemoteImages(vec![(url.to_string(), Some(art()))]),
        );
        assert!(text(&app).unwrap().0.contains('▀'), "the reveal paints");
        assert!(!app.errors.contains_key(&crate::pages::Page::Feed));
    }

    /// A revealed og:image is an own-nest content-addressed blob
    /// (render-model.md § D4), so it rides the SAME by-hash byte path as
    /// `post-image`: the kick collects it, and once the cache holds art the
    /// element repaints as half-block `▀` text instead of the hash placeholder.
    ///
    /// A *blocked* preview contributes no hash — the kick must not fetch bytes
    /// the user has not consented to load, which is the whole of the D3/D4 gate.
    #[test]
    fn link_preview_image_kicks_and_paints_only_once_revealed() {
        let hash = "ab".repeat(32);
        let preview = |revealed: bool| TestLinkPreviewSpec {
            url: "https://example.com/article".to_string(),
            title: "Example Article Title".to_string(),
            description: "A short description.".to_string(),
            image_hash: Some(hash.clone()),
            revealed,
        };
        let preview_post = |revealed: bool| {
            let mut p = post("[https://example.com/article](https://example.com/article)");
            p.link_previews = vec![preview(revealed)];
            p
        };

        // Blocked: no element, and nothing to fetch.
        let mut blocked = feed_app(vec![preview_post(false)]);
        blocked.feed.content = Some(Arc::new(fauna_nest_http::FakeNestContentApi::new()));
        assert!(
            kick_image_fetches(&mut blocked).is_none(),
            "a blocked og:image must not be fetched — the reveal gate is the consent"
        );

        // Revealed: the kick collects the og:image hash…
        let mut app = feed_app(vec![preview_post(true)]);
        app.feed.content = Some(Arc::new(fauna_nest_http::FakeNestContentApi::new()));
        match kick_image_fetches(&mut app) {
            Some(Op::FetchImage { hashes, .. }) => assert_eq!(hashes, vec![hash.clone()]),
            _ => panic!("the kick must request the revealed og:image"),
        }

        // …and the placeholder holds the hash until the art lands.
        let image_text = |app: &App| {
            crate::feed::elements(app)
                .into_iter()
                .find(|e| e.id == "link-preview-image")
                .map(|e| e.text)
        };
        assert_eq!(image_text(&app).as_deref(), Some(hash.as_str()));
        assert!(!image_text(&app).unwrap().contains('▀'));

        apply_outcome(
            &mut app,
            Outcome::Images(vec![(hash.clone(), Finished::Loaded(art()))]),
        );
        assert!(
            image_text(&app).unwrap().contains('▀'),
            "cached art paints the og:image as the element's own characters"
        );

        // A failed fetch degrades to the placeholder, never a blank or a banner.
        let mut failed = feed_app(vec![preview_post(true)]);
        failed.feed.images.set(hash.clone(), None);
        assert_eq!(image_text(&failed).as_deref(), Some(hash.as_str()));
    }

    // ── `post-image` inline paint (Slice 1) ──────────────────────────────────

    /// A feed snapshot with one post whose *resolved* document carries a
    /// `post-image` — a `RenderBlock::Image` the manager's `resolve_media` would
    /// have folded in, which is what makes `first_image_hash()` answer `Some`.
    fn image_post_snapshot(hash: &str) -> FeedSnapshot {
        use fauna_core::render::RenderBlock;
        let mut snapshot = feed_snapshot_with_posts(vec![post("with an image")]);
        snapshot.posts[0].document.blocks.insert(
            0,
            RenderBlock::Image {
                hash: hash.to_string(),
                alt: String::new(),
            },
        );
        snapshot
    }

    fn post_image_text(app: &App) -> Option<String> {
        crate::feed::elements(app)
            .into_iter()
            .find(|e| e.id == "post-image")
            .map(|e| e.text)
    }

    /// Until the bytes load, `post-image` is a placeholder label carrying the
    /// hash; once the cache holds the rasterized art, the SAME element paints as
    /// half-block `▀` text — the positive fetch→paint path the e2e asserts, the
    /// feed twin of `media-thumbnail`'s `painted_thumbnail_count`.
    #[test]
    fn post_image_paints_as_art_once_cached_else_a_placeholder_label() {
        let hash = "cd".repeat(32);
        let mut app = feed_app_with(image_post_snapshot(&hash));

        // No cache entry yet → the placeholder label carries the hash.
        assert_eq!(post_image_text(&app).as_deref(), Some(hash.as_str()));
        assert!(!post_image_text(&app).unwrap().contains('▀'));

        // Art arrives → the element repaints as half-block text.
        app.feed.images.set(hash.clone(), Some(art()));
        assert!(
            post_image_text(&app).unwrap().contains('▀'),
            "cached art paints the picture as the element's own characters"
        );

        // A failed fetch stays on the placeholder, never a blank or an error.
        let mut failed = feed_app_with(image_post_snapshot(&hash));
        failed.feed.images.set(hash.clone(), None);
        assert_eq!(post_image_text(&failed).as_deref(), Some(hash.as_str()));
    }

    /// Every element the feed paints under `id`.
    fn painted<'a>(app: &'a App, id: &'a str) -> Vec<Element> {
        crate::feed::elements(app)
            .into_iter()
            .filter(|e| e.id == id)
            .collect()
    }

    // ── `c2pa-badge` (content-provenance badge) ──────────────────────────────

    /// `c2pa-badge` paints only once the async provenance check resolves
    /// `true`; absent while unchecked/in flight, and absent on a resolved
    /// `false` — the mutation-proof negative
    /// (`test_post_image_shows_c2pa_badge_when_uploaded_with_provenance`'s own
    /// plain-image assertion, mirrored here at unit scope). Scoped under the
    /// card's own `post-card[i]`, the `unverified-source-badge` precedent
    /// (`ui/media.md` § C2PA provenance).
    #[test]
    fn c2pa_badge_paints_on_the_list_card_once_the_check_resolves_true() {
        let hash = "ef".repeat(32);
        let mut app = feed_app_with(image_post_snapshot(&hash));

        assert!(
            painted(&app, "c2pa-badge").is_empty(),
            "no badge until the c2pa check resolves"
        );

        app.feed.c2pa.set(hash.clone(), Some(false));
        assert!(
            painted(&app, "c2pa-badge").is_empty(),
            "an unsigned image must not show a c2pa-badge"
        );

        app.feed.c2pa.set(hash.clone(), Some(true));
        let badges = painted(&app, "c2pa-badge");
        assert_eq!(badges.len(), 1, "one card, at most one badge");
        assert_eq!(
            badges[0].path,
            vec![("post-card".to_string(), 0)],
            "the badge must be scoped under its own post-card"
        );
    }

    /// The same badge on `feed.post_detail` — `post_image_element` is the one
    /// shared wiring point both `post_card` and `post_detail_elements` call, so
    /// a single kick covers both surfaces (the richest existing per-app
    /// pattern: web's shared `C2paImage`/`PostCard` component renders in both
    /// places too, `ui/media.md` § C2PA provenance web note).
    #[test]
    fn c2pa_badge_paints_on_post_detail_too() {
        let hash = "10".repeat(32);
        let snapshot = image_post_snapshot(&hash);
        let post_id = snapshot.posts[0].post_id.clone();
        let mut app = feed_app_with(snapshot);
        apply_local(&mut app, Action::OpenPostDetail(post_id));

        assert!(painted(&app, "c2pa-badge").is_empty());
        app.feed.c2pa.set(hash.clone(), Some(true));
        assert_eq!(painted(&app, "c2pa-badge").len(), 1);
    }

    /// The detail's picture registers INSIDE `feed-post-detail-dialog`, so a
    /// read scoped to the dialog finds it (`post_detail_image_painted`, where a
    /// restricted post's picture first opens). It used to register top-level,
    /// which no scoped query can match.
    #[test]
    fn the_post_detail_s_picture_registers_inside_its_dialog() {
        let hash = "30".repeat(32);
        let snapshot = image_post_snapshot(&hash);
        let post_id = snapshot.posts[0].post_id.clone();
        let mut app = feed_app_with(snapshot);
        apply_local(&mut app, Action::OpenPostDetail(post_id));

        let in_dialog: Vec<_> = painted(&app, "post-image")
            .into_iter()
            .filter(|e| e.path == vec![("feed-post-detail-dialog".to_string(), 0)])
            .collect();
        assert_eq!(
            in_dialog.len(),
            1,
            "the detail's picture must be scoped under its dialog"
        );
        assert!(
            painted(&app, "feed-post-detail-body")
                .iter()
                .all(|e| e.path == vec![("feed-post-detail-dialog".to_string(), 0)]),
            "the detail's body registers inside the dialog too"
        );
    }

    /// [`kick_c2pa_fetches`] requests the check exactly once per hash, and only
    /// for a post that actually has an image — mirrors [`kick_image_fetches`]'s
    /// own dedupe-and-mark-loading gate.
    #[test]
    fn kick_c2pa_fetches_requests_each_hash_once() {
        let hash = "22".repeat(32);
        let mut app = feed_app_with(image_post_snapshot(&hash));
        app.feed.content = Some(Arc::new(fauna_nest_http::FakeNestContentApi::new()));

        match kick_c2pa_fetches(&mut app) {
            Some(Op::FetchC2pa { hashes, .. }) => assert_eq!(hashes, vec![hash.clone()]),
            _ => panic!("the kick must request this post's image hash"),
        }
        assert!(
            app.feed.c2pa.contains(&hash),
            "the kick must mark the hash in-flight so a second tick does not re-request it"
        );
        assert!(
            kick_c2pa_fetches(&mut app).is_none(),
            "an in-flight/resolved hash must not be requested twice"
        );
    }

    /// **The badge is not the uploader's word.** `x-c2pa: true` with bytes that
    /// carry no manifest — exactly what a modified client (or a raw multipart
    /// POST) produces, since the nest stores `UploadSidecar.has_c2pa` for a
    /// public-post blob without ever inspecting the bytes — must resolve
    /// `false`.
    ///
    /// This is the whole defect in
    /// one assertion: painting from stage 1 would make it pass trivially, which
    /// is why the negative comes first and the positive below shares every
    /// other input. `ui/media.md` § C2PA provenance.
    #[tokio::test]
    async fn a_forged_x_c2pa_header_over_plain_bytes_resolves_false() {
        let hash = "33".repeat(32);
        let app = feed_app_with(image_post_snapshot(&hash));
        let content = Arc::new(fauna_nest_http::FakeNestContentApi::new());
        // The lie: the header asserts provenance…
        content.set_c2pa(&paths::blob::by_hash(&hash), Ok(true));
        // …over a PNG with no C2PA manifest in it.
        content.set_ok(
            fauna_nest_http::Verb::Get,
            &paths::blob::by_hash(&hash),
            fauna_media::test_fixtures::build_png(24, 24),
        );

        let outcome = Op::FetchC2pa {
            content,
            manager: app.feed.manager.clone().expect("feed manager present"),
            hashes: vec![hash.clone()],
        }
        .run()
        .await;

        match outcome {
            Outcome::C2pa(results) => assert_eq!(
                results,
                vec![(hash, Finished::Loaded(false))],
                "the bytes carry no manifest, so no badge — whatever the header claims"
            ),
            other => panic!("expected a C2pa outcome, got {other:?}"),
        }
    }

    /// The other half of the pair: a genuinely signed image still resolves
    /// `true`, so the correction cannot be satisfied by simply never painting.
    ///
    /// Ungated on purpose. tui names its own `fauna-media` edge with default
    /// features (`["process_media", "c2pa-detect"]`), so this test reddening
    /// IS the signal that someone took the detector away — the failure mode
    /// the per-artifact feature witness
    /// watches for in the shipped build, caught here in milliseconds instead.
    #[tokio::test]
    async fn a_genuinely_signed_image_still_resolves_true() {
        let hash = "44".repeat(32);
        let app = feed_app_with(image_post_snapshot(&hash));
        let content = Arc::new(fauna_nest_http::FakeNestContentApi::new());
        content.set_c2pa(&paths::blob::by_hash(&hash), Ok(true));
        content.set_ok(
            fauna_nest_http::Verb::Get,
            &paths::blob::by_hash(&hash),
            include_bytes!("../../../../tests/fixtures/c2pa-signed.png").to_vec(),
        );

        let outcome = Op::FetchC2pa {
            content,
            manager: app.feed.manager.clone().expect("feed manager present"),
            hashes: vec![hash.clone()],
        }
        .run()
        .await;

        match outcome {
            Outcome::C2pa(results) => assert_eq!(results, vec![(hash, Finished::Loaded(true))]),
            other => panic!("expected a C2pa outcome, got {other:?}"),
        }
    }

    /// A `false` header ends the check without fetching the bytes — the
    /// pre-filter that keeps the correction from costing a blob GET on every
    /// post in the feed ([`Op::FetchC2pa`] stage 1).
    #[tokio::test]
    async fn a_false_header_short_circuits_before_any_blob_get() {
        let hash = "55".repeat(32);
        let app = feed_app_with(image_post_snapshot(&hash));
        let content = Arc::new(fauna_nest_http::FakeNestContentApi::new());
        content.set_c2pa(&paths::blob::by_hash(&hash), Ok(false));
        // Deliberately NO `set_ok` for the GET: reaching stage 2 here would
        // surface as a fetch failure rather than as this assertion, so the
        // call-count check below is what actually pins the short-circuit.

        let outcome = Op::FetchC2pa {
            content: Arc::clone(&content) as Arc<dyn fauna_nest_http::NestContentApi>,
            manager: app.feed.manager.clone().expect("feed manager present"),
            hashes: vec![hash.clone()],
        }
        .run()
        .await;

        match outcome {
            Outcome::C2pa(results) => {
                assert_eq!(results, vec![(hash.clone(), Finished::Loaded(false))])
            }
            other => panic!("expected a C2pa outcome, got {other:?}"),
        }
        assert!(
            !content
                .calls()
                .iter()
                .any(|(verb, path)| *verb == fauna_nest_http::Verb::Get
                    && path == &paths::blob::by_hash(&hash)),
            "a header that claims nothing must not cost a blob GET: {:?}",
            content.calls()
        );
    }

    /// `post-image` is a BUTTON whose gesture opens the lightbox over *its own*
    /// hash — the wiring constant `test_feed_image_lightbox.py` asserts through
    /// the whole stack, pinned here in 36ms instead of an e2e slot.
    ///
    /// This is the assertion that would have caught the shape it replaced: an
    /// `Element::thumbnail` is `Role::Label`, so before `Element::clickable`
    /// existed a painted picture was *necessarily* inert and no click could ever
    /// have reached a lightbox.
    #[test]
    fn post_image_is_a_button_wired_to_its_own_lightbox() {
        let hash = "cd".repeat(32);
        let app = feed_app_with(image_post_snapshot(&hash));
        let images = painted(&app, "post-image");
        assert_eq!(images.len(), 1, "one card, one post-image");
        match &images[0].role {
            crate::element::Role::Button(Gesture::Feed(Action::OpenLightbox(h))) => {
                assert_eq!(h, &hash, "the click must open THIS post's image")
            }
            other => panic!("post-image must be a lightbox button, got {other:?}"),
        }
    }

    /// Clicking `post-image` paints `image-lightbox`, and it paints the picture
    /// **enlarged** — `LIGHTBOX_COLS` cells wide, not the card's `POST_IMAGE_COLS`
    /// preview. Escape closes it.
    ///
    /// The enlargement is the substance of "full-screen viewer" on a terminal, and
    /// it is the half of the feature an id-presence assertion cannot see: a
    /// lightbox that re-painted the card's art would satisfy the e2e and show the
    /// user nothing new.
    ///
    /// ⚠ The baseline is the SAME pixels rasterized at `POST_IMAGE_COLS`, not the
    /// card element's own text. The first version of this test compared against the
    /// card, and a mutation swapping `LIGHTBOX_COLS` → `POST_IMAGE_COLS` left it
    /// **green**: the `art()` fixture's cached art is one cell wide while its pixels
    /// are re-rasterized to whatever `cols` asks for, so *any* lightbox width beat
    /// the card and the comparison discriminated nothing.
    #[test]
    fn the_lightbox_opens_on_a_post_image_click_and_enlarges_the_picture() {
        let hash = "cd".repeat(32);
        let mut app = feed_app_with(image_post_snapshot(&hash));
        let thumb = art();
        // What the inline card preview's own width would be for this picture.
        let card_cols =
            crate::thumbnail::rasterize_rgb(&thumb.pixels, crate::thumbnail::POST_IMAGE_COLS)
                .expect("the fixture rasterizes")
                .art
                .rows[0]
                .len();
        app.feed.images.set(hash.clone(), Some(thumb));
        assert!(
            painted(&app, "image-lightbox").is_empty(),
            "no lightbox until one is opened"
        );

        assert!(apply_local(&mut app, Action::OpenLightbox(hash.clone())).is_none());
        let lightbox = painted(&app, "image-lightbox");
        assert_eq!(lightbox.len(), 1);
        assert!(
            lightbox[0].text.contains('▀'),
            "the lightbox paints the picture, not a placeholder"
        );
        let lightbox_cols = lightbox[0]
            .text
            .lines()
            .next()
            .expect("the art has a row")
            .chars()
            .count();
        assert!(
            lightbox_cols > card_cols,
            "the lightbox must be BIGGER than the inline card preview \
             ({lightbox_cols} cells vs {card_cols})"
        );

        // Escape is the dismiss affordance (ui.yaml declares no element inside).
        app.handle_key(crate::app::tests::key(crossterm::event::KeyCode::Esc));
        assert!(
            painted(&app, "image-lightbox").is_empty(),
            "Escape closes the lightbox"
        );
    }

    /// A post image whose bytes never arrived still OPENS a lightbox — as the
    /// placeholder the card showed. A click that silently did nothing while the
    /// fetch was in flight is the dropped-action shape testing.md point 11 forbids.
    #[test]
    fn the_lightbox_opens_over_an_unloaded_image_as_the_placeholder() {
        let hash = "cd".repeat(32);
        let mut app = feed_app_with(image_post_snapshot(&hash));
        apply_local(&mut app, Action::OpenLightbox(hash));
        let lightbox = painted(&app, "image-lightbox");
        assert_eq!(lightbox.len(), 1, "the click is honoured, never dropped");
        assert_eq!(lightbox[0].text, crate::thumbnail::PLACEHOLDER);
    }

    /// `compose-dialog-button` opens `feed-compose-dialog`, and the dialog IS the
    /// composer relocated: the same ids move **into** it, so there is never a
    /// second `compose-text-field` for a driver's unscoped read to pick between.
    ///
    /// The relocation is why this needs an assertion beyond "the id appears":
    /// ui.yaml forbids duplicate ids, and the naive shape (paint the dialog's
    /// fields *and* leave the bar's) would register two of each while looking
    /// perfectly correct on screen.
    #[test]
    fn the_compose_dialog_button_opens_the_dialog_and_relocates_the_composer() {
        let mut app = feed_app(vec![post("hello")]);
        assert!(ids(&app).contains("compose-dialog-button"));
        assert!(!ids(&app).contains("feed-compose-dialog"));
        assert_eq!(painted(&app, "compose-text-field").len(), 1);

        assert!(apply_local(&mut app, Action::OpenComposeDialog).is_none());
        assert!(ids(&app).contains("feed-compose-dialog"));
        assert!(
            !ids(&app).contains("compose-dialog-button"),
            "the opener does not sit inside the surface it opened"
        );
        let fields = painted(&app, "compose-text-field");
        assert_eq!(fields.len(), 1, "exactly one composer, never two");
        assert_eq!(
            fields[0].path,
            vec![("feed-compose-dialog".to_string(), 0)],
            "the composer moved INTO the dialog, so scope=feed-compose-dialog resolves it"
        );
        // The submit button came along too — the dialog is a working composer,
        // not a decorative overlay.
        assert_eq!(
            painted(&app, "post-submit-button")[0].path,
            vec![("feed-compose-dialog".to_string(), 0)]
        );

        app.handle_key(crate::app::tests::key(crossterm::event::KeyCode::Esc));
        assert!(
            !ids(&app).contains("feed-compose-dialog"),
            "Escape closes the dialog"
        );
        assert!(ids(&app).contains("compose-dialog-button"));
    }

    /// Posting from the dialog closes it (linux's `d.close()` on its dialog's
    /// Post), and the compose state it leaves behind is the manager's — so a
    /// rejected submit surfaces on the inline bar rather than vanishing with the
    /// overlay.
    #[test]
    fn submitting_closes_the_compose_dialog() {
        let mut app = feed_app(vec![post("hello")]);
        apply_local(&mut app, Action::OpenComposeDialog);
        assert!(app.feed.compose_dialog_open);
        let op = apply_local(&mut app, Action::SubmitPost);
        assert!(matches!(op, Some(Op::SubmitPost { .. })));
        assert!(!app.feed.compose_dialog_open);
        assert!(!ids(&app).contains("feed-compose-dialog"));
    }

    // ── the reply-compose dialog (`feed-reply-dialog`, row 98) ───────────────

    /// `feed-reply-button` arms the dialog (`OpenReplyDialog`), never a bare
    /// `Interact` — reply cannot be a direct tap the way quote/repost/like
    /// are, because `FeedManager::reply` needs a typed body the raw
    /// `interact` door discards (`ui/feed.md` § Implementation status today).
    #[test]
    fn the_reply_button_opens_the_dialog_not_a_bare_interact() {
        let mut app = feed_app(vec![post("hello")]);
        let post_id = app.feed.snapshot().unwrap().posts[0].post_id.clone();

        let button = &painted(&app, "feed-reply-button")[0];
        assert!(
            matches!(&button.role, crate::element::Role::Button(g)
                if matches!(g, Gesture::Feed(Action::OpenReplyDialog(p)) if p == &post_id)),
            "feed-reply-button must arm the dialog, not fire Interact"
        );

        assert!(app.feed.reply_draft.is_none());
        assert!(
            apply_local(&mut app, Action::OpenReplyDialog(post_id.clone())).is_none(),
            "arming the dialog must not dispatch a network op"
        );
        let draft = app.feed.reply_draft.as_ref().expect("dialog armed");
        assert_eq!(draft.post_id, post_id);
        assert_eq!(draft.text, "");
    }

    /// Once armed, the dialog paints its three ui.yaml ids, the two children
    /// scoped under it (the `feed-compose-dialog` `.within()` precedent), the
    /// title names the target's author, and the submit button starts
    /// disabled — an empty reply is meaningless, unlike quote's empty-body
    /// default (ui.yaml's `feed-reply-submit-button` note).
    #[test]
    fn the_reply_dialog_paints_scoped_titled_and_starts_disabled() {
        let mut app = feed_app(vec![post("hello")]);
        let snap = app.feed.snapshot().unwrap();
        let post_id = snap.posts[0].post_id.clone();
        let author = snap.posts[0].author.clone();
        assert!(!ids(&app).contains("feed-reply-dialog"));

        apply_local(&mut app, Action::OpenReplyDialog(post_id));
        let all = ids(&app);
        assert!(all.contains("feed-reply-dialog"));
        assert!(all.contains("feed-reply-text-field"));
        assert!(all.contains("feed-reply-submit-button"));

        let title = &painted(&app, "feed-reply-dialog")[0];
        assert_eq!(title.text, feed::post::replying_to_user(&author));

        let field = &painted(&app, "feed-reply-text-field")[0];
        assert_eq!(field.path, vec![("feed-reply-dialog".to_string(), 0)]);
        let submit = &painted(&app, "feed-reply-submit-button")[0];
        assert_eq!(submit.path, vec![("feed-reply-dialog".to_string(), 0)]);
        assert!(!submit.enabled, "empty text must disable submit");
    }

    /// Typing into `feed-reply-text-field` writes the armed draft's buffer —
    /// page-local, unlike the composer's manager-owned text — and enables
    /// the submit button once the text is non-empty.
    #[test]
    fn reply_text_field_writes_the_armed_drafts_buffer_and_enables_submit() {
        let mut app = feed_app(vec![post("hello")]);
        let post_id = app.feed.snapshot().unwrap().posts[0].post_id.clone();
        apply_local(&mut app, Action::OpenReplyDialog(post_id));

        assert_eq!(field(&app.feed, &FeedField::ReplyText), "");
        set_field(&mut app.feed, FeedField::ReplyText, "totally agree".into());
        assert_eq!(field(&app.feed, &FeedField::ReplyText), "totally agree");
        assert_eq!(app.feed.reply_draft.as_ref().unwrap().text, "totally agree");

        let submit = &painted(&app, "feed-reply-submit-button")[0];
        assert!(submit.enabled, "non-empty text must enable submit");
    }

    /// Writing the field with no dialog armed is a no-op, not a panic — the
    /// same defensive posture every other page-local buffer's setter takes
    /// against a stray write after its surface already closed.
    #[test]
    fn reply_text_field_write_with_no_dialog_armed_is_a_no_op() {
        let mut app = feed_app(vec![post("hello")]);
        assert!(app.feed.reply_draft.is_none());
        set_field(&mut app.feed, FeedField::ReplyText, "orphaned".into());
        assert!(app.feed.reply_draft.is_none());
    }

    /// A whitespace-only submit must not dispatch a reply — the button paints
    /// disabled for exactly this case, and this refuses independently rather
    /// than trusting the paint (`FeedManager::reply` would refuse it too, but
    /// restoring the draft here keeps a stray Enter from silently dropping
    /// typed text, unlike an ordinary rejected submit).
    #[test]
    fn submit_reply_with_only_whitespace_is_a_no_op_and_keeps_the_draft() {
        let mut app = feed_app(vec![post("hello")]);
        let post_id = app.feed.snapshot().unwrap().posts[0].post_id.clone();
        apply_local(&mut app, Action::OpenReplyDialog(post_id));
        set_field(&mut app.feed, FeedField::ReplyText, "   ".into());

        assert!(
            apply_local(&mut app, Action::SubmitReply).is_none(),
            "whitespace-only text must not dispatch a reply"
        );
        assert!(
            app.feed.reply_draft.is_some(),
            "the draft must survive a refused submit"
        );
    }

    /// Submitting dispatches through the shared `FeedManager::reply` door —
    /// never `fauna.posts.interact` — carrying the armed draft's exact target
    /// and body, and closes the dialog optimistically (`SubmitPost`'s own
    /// precedent above: a failure lands on `error-message`, not a slot inside
    /// the now-closed dialog).
    #[test]
    fn submitting_a_reply_dispatches_feedmanager_reply_and_closes_the_dialog() {
        let mut app = feed_app(vec![post("hello")]);
        let post_id = app.feed.snapshot().unwrap().posts[0].post_id.clone();
        apply_local(&mut app, Action::OpenReplyDialog(post_id.clone()));
        set_field(&mut app.feed, FeedField::ReplyText, "totally agree".into());

        let op = apply_local(&mut app, Action::SubmitReply);
        let Some(Op::Reply {
            post_id: pid, body, ..
        }) = op
        else {
            panic!("expected Op::Reply");
        };
        assert_eq!(pid, post_id);
        assert_eq!(body, "totally agree");
        assert!(
            app.feed.reply_draft.is_none(),
            "closes optimistically, same as SubmitPost"
        );
        assert!(!ids(&app).contains("feed-reply-dialog"));
    }

    /// `SubmitReply` issues `fauna.posts.create`, never `fauna.posts.interact`
    /// — the same door `SubmitPost` uses, since a reply is COMPOSED
    /// (`FeedManager::reply` → `compose_referencing_post` →
    /// `PostsClient::posts_create`), not recorded against the target.
    #[test]
    fn submit_reply_wire_kind_is_posts_create_not_interact() {
        assert_eq!(Action::SubmitReply.wire_kind(), Some("fauna.posts.create"));
        assert_eq!(Action::OpenReplyDialog(String::new()).wire_kind(), None);
    }

    /// ui.yaml registers no dismiss element for `feed-reply-dialog` — Escape
    /// is the human-only affordance, the same posture as
    /// `feed-compose-dialog`/`image-lightbox`/`post-tip-list` above.
    #[test]
    fn escape_dismisses_the_reply_dialog() {
        let mut app = feed_app(vec![post("hello")]);
        let post_id = app.feed.snapshot().unwrap().posts[0].post_id.clone();
        apply_local(&mut app, Action::OpenReplyDialog(post_id));
        assert!(ids(&app).contains("feed-reply-dialog"));

        app.handle_key(crate::app::tests::key(crossterm::event::KeyCode::Esc));
        assert!(!ids(&app).contains("feed-reply-dialog"));
        assert!(app.feed.reply_draft.is_none());
    }

    // ── the reply dialog under a restricted target (`ui/feed.md` § Encryption
    //    at rest → *Ruling 5's build — the shape*, (d) + (e)) ────────────────

    /// A restricted post by another author, as the nest projects it — a tier
    /// post, or a room post (the reserved tier `room` plus its channel id).
    fn restricted_post(room: Option<&str>) -> TestPostSpec {
        TestPostSpec {
            gated_tier: Some(room.map_or("patrons", |_| "room").into()),
            gated_room: room.map(str::to_string),
            ..post("for a smaller audience")
        }
    }

    /// The armed dialog's checkbox, or `None` while it is not painted.
    fn public_confirm_checked(app: &App) -> Option<bool> {
        let painted = painted(app, "feed-reply-public-confirm");
        let el = painted.first()?;
        match &el.role {
            crate::element::Role::Checkbox { checked, .. } => Some(*checked),
            other => panic!("feed-reply-public-confirm must be a checkbox, got {other:?}"),
        }
    }

    /// A public target has no audience to state: neither element paints, and
    /// the submit carries no confirmation.
    #[test]
    fn the_reply_dialog_states_no_audience_under_a_public_post() {
        let mut app = feed_app(vec![post("hello")]);
        let post_id = app.feed.snapshot().unwrap().posts[0].post_id.clone();
        apply_local(&mut app, Action::OpenReplyDialog(post_id));
        let all = ids(&app);
        assert!(all.contains("feed-reply-dialog"));
        assert!(!all.contains("feed-reply-audience"));
        assert!(!all.contains("feed-reply-public-confirm"));
    }

    /// **Where the reader cannot write for the audience, the dialog says the
    /// reply would be public and offers the explicit answer** — unchecked at
    /// every open, never remembered — and the submit carries exactly that
    /// answer: unchecked → the ordinary reply (which the manager refuses),
    /// checked → the confirmed-public verb. Both halves are asserted, so a
    /// submit that ignored the checkbox either way reddens this pin (the
    /// confirmation gate's app-side mutation check).
    #[test]
    fn the_reply_dialog_offers_the_public_answer_only_where_the_reader_cannot_write_for_the_audience()
     {
        for target in [
            restricted_post(None),
            restricted_post(Some(&"ee".repeat(32))),
        ] {
            let class = if target.gated_room.is_some() {
                "room"
            } else {
                "tier"
            };
            let mut app = feed_app(vec![target]);
            let snap = app.feed.snapshot().unwrap();
            assert_eq!(
                snap.posts[0].reply_audience,
                Some(ReplyAudience::PublicByConfirmation),
                "{class}: neither seated nor the owner"
            );
            let post_id = snap.posts[0].post_id.clone();

            apply_local(&mut app, Action::OpenReplyDialog(post_id.clone()));
            let audience = &painted(&app, "feed-reply-audience")[0];
            assert_eq!(audience.text, feed::post::REPLY_AUDIENCE_PUBLIC, "{class}");
            assert_eq!(audience.path, vec![("feed-reply-dialog".to_string(), 0)]);
            let confirm = &painted(&app, "feed-reply-public-confirm")[0];
            assert_eq!(confirm.text, feed::post::REPLY_PUBLIC_CONFIRM, "{class}");
            assert_eq!(confirm.path, vec![("feed-reply-dialog".to_string(), 0)]);
            assert_eq!(
                public_confirm_checked(&app),
                Some(false),
                "{class}: unchecked at open"
            );

            // Unchecked: the ordinary reply, never the confirmed verb.
            set_field(&mut app.feed, FeedField::ReplyText, "my words".into());
            let Some(Op::Reply {
                public_confirmed, ..
            }) = apply_local(&mut app, Action::SubmitReply)
            else {
                panic!("{class}: expected Op::Reply");
            };
            assert!(!public_confirmed, "{class}: unchecked sends unconfirmed");

            // Checked: the confirmed verb — and the answer is read at submit.
            apply_local(&mut app, Action::OpenReplyDialog(post_id.clone()));
            assert_eq!(
                public_confirm_checked(&app),
                Some(false),
                "{class}: never remembered"
            );
            assert!(apply_local(&mut app, Action::ToggleReplyPublicConfirm).is_none());
            assert_eq!(public_confirm_checked(&app), Some(true), "{class}");
            set_field(
                &mut app.feed,
                FeedField::ReplyText,
                "my words, public".into(),
            );
            let Some(Op::Reply {
                public_confirmed,
                body,
                ..
            }) = apply_local(&mut app, Action::SubmitReply)
            else {
                panic!("{class}: expected Op::Reply");
            };
            assert!(
                public_confirmed,
                "{class}: checked sends the confirmed verb"
            );
            assert_eq!(body, "my words, public");

            // A third open starts unchecked again.
            apply_local(&mut app, Action::OpenReplyDialog(post_id));
            assert_eq!(
                public_confirm_checked(&app),
                Some(false),
                "{class}: never remembered"
            );
        }
    }

    /// **A seated member's dialog names the room and offers no public
    /// answer**: the reply seals, so there is nothing to confirm — the label
    /// is the composer's own "Room: ‹label›" form, by the reader's own label,
    /// and the submit carries no confirmation.
    #[test]
    fn a_seated_members_reply_dialog_names_the_room_and_offers_no_public_answer() {
        let room = "ee".repeat(32);
        let mut snap = feed_snapshot_with_posts(vec![restricted_post(Some(&room))]);
        snap.own_rooms.push(fauna_feed::GateRoomOption {
            room: room.clone(),
            label: "Garden".into(),
        });
        let mut app = feed_app_with(snap);
        let snap = app.feed.snapshot().unwrap();
        assert_eq!(
            snap.posts[0].reply_audience,
            Some(ReplyAudience::SealedToRoom)
        );
        let post_id = snap.posts[0].post_id.clone();

        apply_local(&mut app, Action::OpenReplyDialog(post_id));
        let audience = &painted(&app, "feed-reply-audience")[0];
        assert_eq!(audience.text, feed::post::reply_audience_room("Garden"));
        assert_eq!(audience.path, vec![("feed-reply-dialog".to_string(), 0)]);
        assert!(!ids(&app).contains("feed-reply-public-confirm"));
        // Nothing to toggle: the answer cannot be given where it is not asked.
        assert!(apply_local(&mut app, Action::ToggleReplyPublicConfirm).is_none());
        set_field(
            &mut app.feed,
            FeedField::ReplyText,
            "said in the room".into(),
        );
        let Some(Op::Reply {
            public_confirmed, ..
        }) = apply_local(&mut app, Action::SubmitReply)
        else {
            panic!("expected Op::Reply");
        };
        assert!(!public_confirmed);
    }

    /// Flipping the answer is local — nothing is sent until the submit.
    #[test]
    fn toggle_reply_public_confirm_is_local() {
        assert_eq!(Action::ToggleReplyPublicConfirm.wire_kind(), None);
    }

    /// The kick requests every uncached `post-image` hash exactly once and marks
    /// it in flight, so a burst of `FeedChanged` ticks costs one fetch — the feed
    /// twin of `the_kick_requests_each_uncached_hash_exactly_once`.
    #[test]
    fn kick_image_fetches_requests_each_uncached_image_once() {
        let hash = "ef".repeat(32);
        let mut app = feed_app_with(image_post_snapshot(&hash));
        // The kick gates on the HTTP/bulk plane the Op fetches over — present
        // after auth in production; the test double stands in for it here.
        app.feed.content = Some(Arc::new(fauna_nest_http::FakeNestContentApi::new()));

        match kick_image_fetches(&mut app) {
            Some(Op::FetchImage { hashes, .. }) => assert_eq!(hashes, vec![hash.clone()]),
            _ => panic!("the kick must produce a post-image fetch"),
        }
        assert!(
            matches!(app.feed.images.get(&hash), Some(ImageState::Loading)),
            "the requested hash is marked in flight"
        );
        // A repeat tick with the same snapshot has nothing to do — the `Loading`
        // mark is what makes the repeat impossible.
        assert!(
            kick_image_fetches(&mut app).is_none(),
            "a repeat tick must not re-issue an in-flight fetch"
        );
        // Nor once it settles as art or as a failure.
        apply_outcome(
            &mut app,
            Outcome::Images(vec![(hash.clone(), Finished::Loaded(art()))]),
        );
        assert!(
            kick_image_fetches(&mut app).is_none(),
            "a settled fetch is never re-issued"
        );
    }

    /// Run one `post-image` fetch for `hash` and fold it — the kick →
    /// `Op::run` → `apply_outcome` round a `FeedChanged` tick drives.
    async fn fetch_and_fold(app: &mut App, hash: &str) {
        let op = kick_image_fetches(app).expect("the kick must request the image");
        let outcome = op.run().await;
        assert!(matches!(&outcome, Outcome::Images(arts) if arts[0].0 == hash));
        apply_outcome(app, outcome);
    }

    /// render-model.md § D6c: a bridged post's own picture (`RenderBlock::ProxiedImage`)
    /// paints in the `post-image` slot through the SAME loader, with its nest-relative
    /// path as the `content.get` argument (the bearer-carrying bulk plane): placeholder
    /// label = the path, kick → `Op::FetchImage { paths }` → the nest's bytes → art,
    /// and the art paints immediately — there is no reveal gate on it.
    #[tokio::test]
    async fn a_bridged_proxied_image_paints_in_post_image_through_the_nest() {
        use fauna_core::render::RenderBlock;
        let path = "/api/v1/media/proxy?url=https%3A%2F%2Ffiles.example%2Fa.png".to_string();
        let mut snapshot = feed_snapshot_with_posts(vec![post("bridged")]);
        snapshot.posts[0].media_hash = Some(String::new());
        snapshot.posts[0]
            .document
            .blocks
            .push(RenderBlock::ProxiedImage {
                path: path.clone(),
                alt: String::new(),
            });
        let mut app = feed_app_with(snapshot);
        let content = Arc::new(fauna_nest_http::FakeNestContentApi::new());
        app.feed.content = Some(Arc::clone(&content) as Arc<dyn NestContentApi>);

        assert_eq!(post_image_text(&app).as_deref(), Some(path.as_str()));

        content.set_ok(
            fauna_nest_http::Verb::Get,
            &path,
            fauna_media::test_fixtures::build_png(4, 4),
        );
        let op = kick_image_fetches(&mut app).expect("the kick must request the picture");
        match &op {
            Op::FetchImage { hashes, paths, .. } => {
                assert!(hashes.is_empty(), "no blob fetch for a bridged picture");
                assert_eq!(paths, &vec![path.clone()]);
            }
            _ => panic!("expected Op::FetchImage"),
        }
        apply_outcome(&mut app, op.run().await);
        assert_eq!(
            content.calls(),
            vec![(fauna_nest_http::Verb::Get, path.clone())],
            "fetched from this nest at the proxied path, nothing else"
        );
        assert!(
            post_image_text(&app).unwrap().contains('▀'),
            "the proxied picture paints as art in the post-image slot"
        );
        assert!(
            kick_image_fetches(&mut app).is_none(),
            "a settled picture is never re-fetched"
        );
    }

    /// `ui/feed.md` § Implementation status today, `SourceKind::Bridged`: a
    /// post whose source token names a bridge on the snapshot's roster paints
    /// that bridge's declared glyph and label; with no roster entry the same
    /// token is an unknown source, glyph alone.
    #[test]
    fn a_bridged_posts_badge_names_its_bridge_from_the_roster() {
        use fauna_core::source_glyph::{BridgeIdentitySnapshot, SourceGlyph};
        let badges = |app: &App| -> Vec<String> {
            crate::feed::elements(app)
                .into_iter()
                .filter(|e| e.id == "protocol-badge")
                .map(|e| e.text)
                .collect()
        };
        let spec = || TestPostSpec {
            source: "fauna, matrix".to_string(),
            ..post("carried over a bridge")
        };
        let mut snapshot = feed_snapshot_with_posts(vec![spec()]);
        snapshot.bridge_roster = vec![BridgeIdentitySnapshot {
            id: "matrix".into(),
            label: "Matrix".into(),
            glyph: SourceGlyph::Globe,
        }];
        assert_eq!(
            badges(&feed_app_with(snapshot)),
            vec![
                SourceGlyph::Fox.emoji().to_string(),
                format!("{} Matrix", SourceGlyph::Globe.emoji()),
            ]
        );
        assert_eq!(
            badges(&feed_app_with(feed_snapshot_with_posts(vec![spec()]))),
            vec![
                SourceGlyph::Fox.emoji().to_string(),
                SourceGlyph::Unknown.emoji().to_string(),
            ],
            "an unlisted token stays an unknown source"
        );
    }

    /// render-model.md § D6c → *Proxied video*: a bridged post's own video
    /// (`RenderBlock::ProxiedVideo`) paints in the `video-thumbnail` slot as the
    /// play glyph + its proxied path — and NO byte fetch is kicked for it (no app
    /// byte-loads a proxied video for its thumbnail; there is no poster to load).
    #[test]
    fn a_bridged_proxied_video_paints_its_path_in_video_thumbnail_and_fetches_nothing() {
        use fauna_core::render::RenderBlock;
        let path = "/api/v1/media/proxy?url=https%3A%2F%2Ffiles.example%2Fa.mp4".to_string();
        let mut snapshot = feed_snapshot_with_posts(vec![post("bridged")]);
        snapshot.posts[0].media_hash = Some(String::new());
        snapshot.posts[0]
            .document
            .blocks
            .push(RenderBlock::ProxiedVideo {
                path: path.clone(),
                alt: String::new(),
            });
        let mut app = feed_app_with(snapshot);
        let content = Arc::new(fauna_nest_http::FakeNestContentApi::new());
        app.feed.content = Some(Arc::clone(&content) as Arc<dyn NestContentApi>);

        let thumb = crate::feed::elements(&app)
            .into_iter()
            .find(|e| e.id == "video-thumbnail")
            .map(|e| e.text);
        assert_eq!(thumb, Some(format!("▶ {path}")));
        assert_eq!(post_image_text(&app), None, "never painted as an image");
        assert!(
            kick_image_fetches(&mut app).is_none(),
            "no byte fetch for a proxied video's thumbnail"
        );
        assert!(content.calls().is_empty());
    }

    /// A nest that could not serve the image right now (a timeout, a `503`
    /// under load) must not blank it for the rest of the session: the failure
    /// is forgotten, so the next tick's kick asks again — and a later success
    /// paints (`fauna_core::load_cache`'s module doc, transient failures).
    #[tokio::test]
    async fn a_transient_image_fetch_failure_is_retried_on_the_next_kick() {
        let hash = "a1".repeat(32);
        let path = paths::blob::by_hash(&hash);
        let mut app = feed_app_with(image_post_snapshot(&hash));
        let content = Arc::new(fauna_nest_http::FakeNestContentApi::new());
        app.feed.content = Some(Arc::clone(&content) as Arc<dyn NestContentApi>);

        content.set_status(fauna_nest_http::Verb::Get, &path, 503, "overloaded");
        fetch_and_fold(&mut app, &hash).await;
        assert!(
            app.feed.images.get(&hash).is_none(),
            "a transient failure records nothing"
        );

        content.set_ok(
            fauna_nest_http::Verb::Get,
            &path,
            fauna_media::test_fixtures::build_png(4, 4),
        );
        fetch_and_fold(&mut app, &hash).await;
        assert!(matches!(
            app.feed.images.get(&hash),
            Some(ImageState::Ready(_))
        ));
    }

    /// The other half: the nest's own answer about the blob (a `404`) is
    /// terminal — the next tick must not ask again.
    #[tokio::test]
    async fn a_missing_image_is_settled_and_never_refetched() {
        let hash = "a2".repeat(32);
        let mut app = feed_app_with(image_post_snapshot(&hash));
        let content = Arc::new(fauna_nest_http::FakeNestContentApi::new());
        content.set_status(
            fauna_nest_http::Verb::Get,
            &paths::blob::by_hash(&hash),
            404,
            "no such blob",
        );
        app.feed.content = Some(content as Arc<dyn NestContentApi>);

        fetch_and_fold(&mut app, &hash).await;
        assert!(matches!(
            app.feed.images.get(&hash),
            Some(ImageState::Failed)
        ));
        assert!(kick_image_fetches(&mut app).is_none());
    }

    /// A C2PA check the nest could not answer right now leaves the verdict
    /// unsettled rather than a cached `false`, so the badge can still appear.
    #[tokio::test]
    async fn a_transient_c2pa_check_failure_leaves_the_verdict_unsettled() {
        let hash = "a3".repeat(32);
        let app = feed_app_with(image_post_snapshot(&hash));
        let content = Arc::new(fauna_nest_http::FakeNestContentApi::new());
        content.set_c2pa(
            &paths::blob::by_hash(&hash),
            Err(fauna_nest_http::ApiError::Transport("timed out".into())),
        );

        let outcome = Op::FetchC2pa {
            content,
            manager: app.feed.manager.clone().expect("feed manager present"),
            hashes: vec![hash.clone()],
        }
        .run()
        .await;

        match outcome {
            Outcome::C2pa(results) => assert_eq!(results, vec![(hash, Finished::Transient)]),
            other => panic!("expected a C2pa outcome, got {other:?}"),
        }
    }

    /// Folding `Outcome::Images` writes art / failure into the cache and never
    /// touches the page banner in either direction — one unreadable image is a
    /// placeholder, not an error the user must read (mirrors Media's thumbnails).
    #[test]
    fn folding_images_writes_art_and_failure_without_touching_the_banner() {
        let mut app = feed_app(vec![]);
        // A prior, unrelated compose error must survive an image fold.
        app.errors
            .insert(crate::pages::Page::Feed, "prior error".into());

        apply_outcome(
            &mut app,
            Outcome::Images(vec![
                ("h1".into(), Finished::Loaded(art())),
                ("h2".into(), Finished::Failed),
            ]),
        );

        assert!(matches!(
            app.feed.images.get("h1"),
            Some(ImageState::Ready(_))
        ));
        assert!(matches!(
            app.feed.images.get("h2"),
            Some(ImageState::Failed)
        ));
        assert_eq!(
            app.errors
                .get(&crate::pages::Page::Feed)
                .map(String::as_str),
            Some("prior error"),
            "an image fold must not clear an unrelated error"
        );
    }

    /// ui.yaml `feed` transition `click post-card → post_detail`: clicking a
    /// card opens the sub-page showing that post's author + body, and nothing
    /// else from `the_feed_page_registers_only_ui_yaml_ids`'s set leaks through.
    #[test]
    fn opening_post_detail_shows_the_clicked_posts_author_and_body() {
        let p = post("detail body **bold**");
        let post_id = p.post_id.clone();
        let mut app = feed_app(vec![p]);

        // The sub-page opens locally; the returned op only resolves/unseals,
        // and is a no-op for a public post the timeline already holds.
        apply_local(&mut app, Action::OpenPostDetail(post_id.clone()));
        assert_eq!(app.feed.mode, Mode::PostDetail(post_id));

        let elements = crate::feed::elements(&app);
        assert!(elements.iter().any(|e| e.id == "feed-post-detail-dialog"));
        let text = |id: &str| {
            elements
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.text.clone())
                .unwrap_or_default()
        };
        // Through the one resolver: no nickname and no handle on a feed
        // author, so the canonical short id (value-formatting.md § Peer
        // display label).
        assert_eq!(
            text("feed-post-detail-author"),
            fauna_core::format::short_id(&"bb".repeat(32))
        );
        let body = text("feed-post-detail-body");
        assert!(body.contains("bold"), "got {body:?}");
        assert!(
            !body.contains("**"),
            "the body is walked, not raw markdown: {body:?}"
        );
        assert!(
            !elements.iter().any(|e| e.id == "post-card"),
            "post_detail replaces the list, like create_feed"
        );
    }

    /// A bridged author's face (bridges.md § Unified feed ingestion →
    /// *Bridged authors*) rides the one resolver: the display name leads the
    /// card and the detail, a face with only a handle paints the handle, and
    /// a native author still reads as the canonical short id — no per-bridge
    /// label code anywhere on this app.
    #[test]
    fn a_bridged_authors_face_names_the_card_and_the_detail() {
        use fauna_feed::AuthorDisplayView;
        let named = TestPostSpec {
            post_id: "a1".repeat(32),
            author: "b1".repeat(32),
            source: "activitypub".into(),
            author_display: Some(AuthorDisplayView {
                handle: Some("@bob@remote.example".into()),
                display_name: Some("Bob".into()),
                avatar_url: None,
            }),
            ..post("from the fediverse")
        };
        let handle_only = TestPostSpec {
            post_id: "a2".repeat(32),
            author: "b2".repeat(32),
            source: "bluesky".into(),
            author_display: Some(AuthorDisplayView {
                handle: Some("alice.bsky.social".into()),
                display_name: None,
                avatar_url: None,
            }),
            ..post("from bluesky")
        };
        let native = post("from home");
        let mut app = feed_app(vec![named, handle_only, native]);

        let authors: Vec<String> = crate::feed::elements(&app)
            .into_iter()
            .filter(|e| e.id == "post-author")
            .map(|e| e.text)
            .collect();
        assert_eq!(
            authors,
            vec![
                "Bob".to_string(),
                "alice.bsky.social".to_string(),
                fauna_core::format::short_id(&"bb".repeat(32)),
            ]
        );

        apply_local(&mut app, Action::OpenPostDetail("a1".repeat(32)));
        let detail = crate::feed::elements(&app)
            .into_iter()
            .find(|e| e.id == "feed-post-detail-author")
            .map(|e| e.text)
            .unwrap_or_default();
        assert_eq!(detail, "Bob");
    }

    /// `Outcome::Done` now clears a prior page error, unifying with the six
    /// other `Outcome`-shaped pages (each clears on its own success variant —
    /// `settings::Outcome::FilterCreated`, `events::Outcome::EventMutated`, …).
    /// Every feed mutation collapses into this one success variant, so success
    /// on ANY gesture clears a stale error from an unrelated prior failure —
    /// exactly the existing cross-page contract (`app.errors` is page-scoped,
    /// not gesture-scoped).
    #[test]
    fn outcome_done_clears_a_prior_error() {
        let mut app = feed_app(vec![post("hello")]);
        app.errors
            .insert(Page::Feed, "a stale load-more failure".to_string());
        apply_outcome(&mut app, Outcome::Done);
        assert!(
            !app.errors.contains_key(&Page::Feed),
            "a successful op must clear a prior page error"
        );
    }

    // ── row 31/row 32: bridge-feed subscribe + feed-delete-button ──────────

    fn feed_app_with_feeds(feeds: Vec<FeedSummaryView>) -> App {
        feed_app_with(FeedSnapshot {
            feeds,
            status: fauna_feed::FeedStatus::Loaded,
            ..Default::default()
        })
    }

    fn feed_app_with_bridges(
        available: Vec<fauna_feed::AvailableBridge>,
        bridge_feeds: Vec<fauna_feed::BridgeFeedView>,
    ) -> App {
        feed_app_with(FeedSnapshot {
            available_bridges: available,
            bridge_feeds,
            status: fauna_feed::FeedStatus::Loaded,
            ..Default::default()
        })
    }

    /// `feed-delete-button` paints once per `feed-item`, scoped under it
    /// (`feed-post-actions-button`'s `.within(ids::POST_CARD, i)` idiom), and
    /// wires `Action::DeleteFeed` to that row's own feed id — never the wrong
    /// row's when two feeds are loaded.
    #[test]
    fn feed_delete_button_paints_per_row_and_wires_the_right_feed_id() {
        let app = feed_app_with_feeds(vec![
            FeedSummaryView {
                feed_id: "feed-a".into(),
                name: "A".into(),
                combination: "all".into(),
                scope: String::new(),
                contributor_seeds: Vec::new(),
            },
            FeedSummaryView {
                feed_id: "feed-b".into(),
                name: "B".into(),
                combination: "all".into(),
                scope: String::new(),
                contributor_seeds: Vec::new(),
            },
        ]);
        let elements = crate::feed::elements(&app);
        let delete_buttons: Vec<_> = elements
            .iter()
            .filter(|e| e.id == "feed-delete-button")
            .collect();
        assert_eq!(delete_buttons.len(), 2, "one per feed row");
        for (i, btn) in delete_buttons.iter().enumerate() {
            assert_eq!(
                btn.path.first().map(|(id, idx)| (id.as_str(), *idx)),
                Some(("feed-item", i)),
                "scoped under its own feed-item row"
            );
        }
        let want_ids = ["feed-a", "feed-b"];
        for (btn, want) in delete_buttons.iter().zip(want_ids) {
            match &btn.role {
                crate::element::Role::Button(Gesture::Feed(Action::DeleteFeed(id))) => {
                    assert_eq!(id, want)
                }
                other => panic!("expected DeleteFeed({want}), got {other:?}"),
            }
        }
    }

    #[test]
    fn delete_feed_action_dispatches_the_delete_feed_op() {
        let mut app = feed_app_with_feeds(vec![FeedSummaryView {
            feed_id: "feed-a".into(),
            name: "A".into(),
            combination: "all".into(),
            scope: String::new(),
            contributor_seeds: Vec::new(),
        }]);
        let op = apply_local(&mut app, Action::DeleteFeed("feed-a".into()));
        assert!(matches!(op, Some(Op::DeleteFeed { feed_id, .. }) if feed_id == "feed-a"));
    }

    /// `version-compatibility.md` § Dim 3: the subscribe affordance must not
    /// offer a bridge the nest can't serve.
    #[test]
    fn bridge_subscribe_toggle_hidden_when_no_bridges_available() {
        let app = feed_app_with_bridges(Vec::new(), Vec::new());
        assert!(!ids(&app).contains("bridge-feed-subscribe-toggle"));
    }

    #[test]
    fn bridge_subscribe_toggle_shown_when_a_bridge_is_available() {
        let app = feed_app_with_bridges(
            vec![fauna_feed::AvailableBridge {
                id: "nostr".into(),
                name: "Nostr".into(),
            }],
            Vec::new(),
        );
        assert!(ids(&app).contains("bridge-feed-subscribe-toggle"));
        // The inline form is closed until the toggle is clicked.
        assert!(!ids(&app).contains("bridge-form-uri-input"));
    }

    /// `OpenBridgeForm` opens the inline form and pre-selects the first
    /// available bridge (`CreateFeedForm::fresh`'s rule-type precedent);
    /// `CancelBridgeForm` closes it again with no network call.
    #[test]
    fn open_and_cancel_bridge_form_round_trip_with_no_network_call() {
        let mut app = feed_app_with_bridges(
            vec![fauna_feed::AvailableBridge {
                id: "nostr".into(),
                name: "Nostr".into(),
            }],
            Vec::new(),
        );
        assert!(apply_local(&mut app, Action::OpenBridgeForm).is_none());
        assert!(app.feed.bridge_form_open);
        assert_eq!(app.feed.bridge_form.kind, "nostr");
        let elements = crate::feed::elements(&app);
        for id in [
            "bridge-form-bridge-select",
            "bridge-form-uri-input",
            "bridge-form-name-input",
            "bridge-form-subscribe-button",
            "bridge-form-cancel-button",
        ] {
            assert!(
                elements.iter().any(|e| e.id == id),
                "{id} should paint while the form is open"
            );
        }

        assert!(apply_local(&mut app, Action::CancelBridgeForm).is_none());
        assert!(!app.feed.bridge_form_open);
        assert!(!ids(&app).contains("bridge-form-uri-input"));
    }

    /// Submitting forwards the buffered fields to `Op::SubscribeBridge` and
    /// closes the form optimistically (the `Action::CreateFeed` precedent) —
    /// a failure surfaces on the page-level `error-message` via the shared
    /// `Outcome::Error` path, not a form-scoped slot ui.yaml doesn't define.
    #[test]
    fn subscribe_bridge_submits_the_buffered_fields_and_closes_the_form() {
        let mut app = feed_app_with_bridges(
            vec![fauna_feed::AvailableBridge {
                id: "nostr".into(),
                name: "Nostr".into(),
            }],
            Vec::new(),
        );
        apply_local(&mut app, Action::OpenBridgeForm);
        app.feed.bridge_form.uri = "nostr://npub1example".into();
        app.feed.bridge_form.name = "My Nostr Feed".into();

        let op = apply_local(&mut app, Action::SubscribeBridge);
        assert!(
            matches!(
                &op,
                Some(Op::SubscribeBridge { kind, uri, name, .. })
                    if kind == "nostr" && uri == "nostr://npub1example" && name == "My Nostr Feed"
            ),
            "expected SubscribeBridge with the buffered fields"
        );
        assert!(
            !app.feed.bridge_form_open,
            "submit closes the form optimistically, like CreateFeed"
        );
    }

    /// A subscribed bridge feed paints an unsubscribe control that wires back
    /// to its own row id — `bridge-feed-unsubscribe-button` is a real indexed
    /// element (`ui.yaml`), so it paints plain, the same idiom `feed-item`
    /// uses for its own top-level index.
    #[test]
    fn unsubscribe_bridge_wires_the_rows_own_id() {
        let mut app = feed_app_with_bridges(
            Vec::new(),
            vec![fauna_feed::BridgeFeedView {
                id: 42,
                bridge: "nostr".into(),
                feed_uri: "nostr://npub1example".into(),
                name: "My Nostr Feed".into(),
            }],
        );
        let elements = crate::feed::elements(&app);
        let btn = elements
            .iter()
            .find(|e| e.id == "bridge-feed-unsubscribe-button")
            .expect("painted");
        match &btn.role {
            crate::element::Role::Button(Gesture::Feed(Action::UnsubscribeBridge(id))) => {
                assert_eq!(*id, 42)
            }
            other => panic!("expected UnsubscribeBridge(42), got {other:?}"),
        }

        let op = apply_local(&mut app, Action::UnsubscribeBridge(42));
        assert!(matches!(op, Some(Op::UnsubscribeBridge { id: 42, .. })));
    }

    /// A subscription surviving even though its bridge is no longer available
    /// must stay unsubscribe-able —
    /// the two lists are independent, never cross-gated.
    #[test]
    fn existing_subscription_stays_manageable_when_its_bridge_is_no_longer_available() {
        let app = feed_app_with_bridges(
            Vec::new(),
            vec![fauna_feed::BridgeFeedView {
                id: 1,
                bridge: "nostr".into(),
                feed_uri: "nostr://npub1example".into(),
                name: "My Nostr Feed".into(),
            }],
        );
        assert!(ids(&app).contains("bridge-feed-unsubscribe-button"));
        assert!(
            !ids(&app).contains("bridge-feed-subscribe-toggle"),
            "no available bridge left to subscribe to another"
        );
    }
}
