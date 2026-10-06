//! Encrypted-mode author-side **mint + upload orchestration** — the high-level
//! `create_tier` / `approve_subscriber` / `remove_subscriber` calls that wrap
//! the thin [`SubscriptionsClient`] kinds with the broadcast-`KeyBlob` crypto
//! dance so all 7 apps dispatch **one** call instead of re-implementing the
//! mint per client (priority #2; `docs/goal/behavior/monetization.md` § Pillar 1
//! — *Where the logic lives*).
//!
//! ## The only key path
//!
//! The nest holds no period key, so the author's client mints the
//! roster-covering `KeyBlob` and ships it in the `encrypted_upload` envelope
//! (`approve` / `subscribers.remove` / `key_blob.rotate`); the nest refuses
//! those doors without one (`missing_upload`). This module is that path, on
//! every nest.
//!
//! ## What each call does
//!
//! - **`create_tier`** records a fresh period key in custody (the
//!   `fauna.state.subscriptions` plane rows, through [`crate::period_keys::PeriodKeyStore`]),
//!   then creates the tier server-side. No `KeyBlob` (empty roster).
//! - **`approve_subscriber`** reads the post-approval roster (current ∪ new),
//!   mints the blob over the tier's `current` period key (no rotation — adding a
//!   member doesn't break forward secrecy), and uploads via `requests.approve`.
//! - **`remove_subscriber`** *rotates* to a fresh period key (forward secrecy:
//!   the leaver must not read future content), mints over the post-removal
//!   roster (current ∖ removed), and uploads via `subscribers.remove`.
//!
//! ## Crash-safety (the load-bearing design)
//!
//! A removal's fresh period key is **irrecoverable** once the nest stores the
//! re-wrapped blob: lose it after the upload and the author can neither decrypt
//! their own future content nor mint for new subscribers in that period — a
//! no-user-data-loss violation. So a removal **stages** the new period as a
//! `removal/` row and **persists it before the upload**, committing it into
//! the tier's `current` — and settling the staging — only once the nest
//! confirms. A crash between upload and commit is healed by
//! [`SubscriptionsAuthor::resume_pending_removals`], which re-drives the staged
//! key idempotently (a removal the nest already applied resolves via the
//! `not_subscribed` path). This is the same resume-sentinel shape as
//! `fauna-client-mail-settings`'s MSEK rotation (`rotation.rs`), which is why
//! this orchestration **owns** persistence (a [`crate::period_keys::PeriodKeyStore`]) rather than
//! taking a replica — only an owner can land the sentinel mid-flight.
//!
//! Retry: a concurrent device approving/removing under us surfaces a nest
//! `roster_mismatch` (the live roster moved) or `stale_rotation` (our blob's
//! `rotated_at` no longer beats the stored one). Both are retried with a
//! re-read roster + advanced `rotated_at`, bounded by [`MAX_MINT_ATTEMPTS`]. The
//! exact wire codes are matched generically via [`RpcErrorClass::as_rpc_error`]
//! (the `fauna-client-conversations` pattern), so the loop is transport-agnostic
//! (native `NestClientError` / wasm `WsRpcError`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use fauna_core::data::{PendingRemoval, TierPeriod, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, canonical_decode, decode_signed_bytes};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::localized::LocalizedText;
use fauna_core::progress::ProgressOutcome;
use fauna_core::subscription::crypto::{
    MLKEM768_ENCAPS_KEY_LEN, MintError, SelfDelegationError,
    build_manage_subscribers_self_delegation, mint_key_blob,
};
use fauna_core::subscription::types::KeyBlob;
use fauna_core::subscription::{
    FOLLOWERS_TIER, FOLLOWERS_TIER_RANK, OWNER_ONLY_TIER, OWNER_ONLY_TIER_RANK,
};
use fauna_protocol::discovery::capability;
use fauna_protocol::subscriptions::{
    ApproveRequestReply, EncryptedKeyBlobUpload, PendingRequest, TierAskingPrice,
};
use fauna_protocol::{RpcErrorClass, RpcRequester};
use rand::RngCore;
use zeroize::Zeroizing;

use crate::SubscriptionsClient;
use crate::custody::{self, CustodyError};
use crate::period_keys::SharedPeriodKeyStore;
use fauna_client_config::StoreError;

/// Retryable nest rejection: the live roster moved between our `subscribers.list`
/// read and the upload.
const ROSTER_MISMATCH: &str = "fauna.subscriptions.roster_mismatch";
/// Retryable nest rejection: our blob's `rotated_at` does not strictly exceed
/// the stored blob's (a concurrent rotation, or clock skew).
const STALE_ROTATION: &str = "fauna.subscriptions.stale_rotation";
/// Terminal-success for a removal: the subscriber is already gone (a prior,
/// crash-interrupted attempt of this very removal already applied it).
const NOT_SUBSCRIBED: &str = "fauna.subscriptions.not_subscribed";

/// What one [`SubscriptionsAuthor::rotate_period_keys_after_succession`] pass
/// did. Reported rather than returned as a bare count because the three "did
/// nothing" arms need different answers from a surface: nothing held is the
/// ordinary state, a nest that cannot rotate is a **still-open exposure**
/// waiting on a nest update, and a swept pass with failures is one waiting on
/// a retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodRotationOutcome {
    /// The author holds no client-minted period key for any tier — every
    /// identity that never created an encrypted-mode tier lands here, as does
    /// a plaintext-mode account.
    NothingHeld,
    /// This nest does not advertise
    /// [`capability::SUBSCRIPTION_PERIOD_ROTATE`], so there is no door to
    /// publish the rotated blob through and the pass deliberately did nothing.
    /// ⚠ The predecessor-era key stays live until the nest is updated.
    NestCannotRotate,
    /// The pass ran over every held tier.
    Swept {
        /// Tiers re-keyed and republished by this pass.
        rotated: usize,
        /// Tiers whose stored blob already named this identity — nothing owed,
        /// which is what every pass after the first reports.
        already_current: usize,
        /// Tiers that could not be completed; the next pass re-derives them.
        failed: usize,
    },
}

/// The rotation as a **progress surface**, mirroring `MailBurnProgress` and
/// its siblings arm for arm via the shared [`fauna_core::progress::Passage`] —
/// the projection all 7 apps render, so the copy lives where the outcome lives
/// and no app writes a `match` over the outcome.
pub type PeriodRotationProgress = fauna_core::progress::Passage<PeriodRotationOutcome>;

/// i18n keys for [`PeriodRotationProgress::status_line`] —
/// `settings.recovery_kit.*`, beside the other aftermath legs' lines.
const KEY_ROTATE_RUNNING: &str = "settings.recovery_kit.tier_period_rotation_running";
const KEY_ROTATE_DONE: &str = "settings.recovery_kit.tier_period_rotation_done";
const KEY_ROTATE_NEST_TOO_OLD: &str = "settings.recovery_kit.tier_period_rotation_nest_too_old";
const KEY_ROTATE_PARTIAL: &str = "settings.recovery_kit.tier_period_rotation_partial";
const KEY_ROTATE_FAILED: &str = "settings.recovery_kit.tier_period_rotation_failed";

/// Two arms render and three do not, on the sibling legs' rule: say something
/// exactly when the user has to know something.
///
/// [`PeriodRotationOutcome::NestCannotRotate`] is the arm that matters most and
/// the least obvious: nothing failed, so a surface that stayed quiet would
/// report a clean aftermath while the predecessor's key still opens everything
/// the successor publishes to that tier. It names the remedy (update the nest)
/// because the user cannot otherwise discover that one exists.
impl ProgressOutcome for PeriodRotationOutcome {
    const RUNNING_KEY: &'static str = KEY_ROTATE_RUNNING;
    const FAILED_KEY: &'static str = KEY_ROTATE_FAILED;

    fn settled_line(&self) -> Option<LocalizedText> {
        match self {
            Self::NestCannotRotate => Some(LocalizedText::key(KEY_ROTATE_NEST_TOO_OLD)),
            Self::Swept {
                rotated, failed, ..
            } if *failed > 0 => {
                let mut text = LocalizedText::key(KEY_ROTATE_PARTIAL);
                text.args.insert("count".into(), rotated.to_string());
                text.args.insert("failed".into(), failed.to_string());
                Some(text)
            }
            Self::Swept { rotated, .. } if *rotated > 0 => {
                let mut text = LocalizedText::key(KEY_ROTATE_DONE);
                text.args.insert("count".into(), rotated.to_string());
                Some(text)
            }
            // Nothing held, or every tier already on a successor-minted key —
            // which is what every pass after the first reports.
            Self::NothingHeld | Self::Swept { .. } => None,
        }
    }

    /// Still owed while a nest cannot serve the door, or while any tier failed
    /// — both leave a predecessor-era key live on at least one tier.
    fn still_owed(&self) -> bool {
        match self {
            Self::NestCannotRotate => true,
            Self::Swept { failed, .. } => *failed > 0,
            Self::NothingHeld => false,
        }
    }
}

impl PeriodRotationOutcome {
    fn count_rotated(&mut self) {
        if let Self::Swept { rotated, .. } = self {
            *rotated += 1;
        }
    }
    fn count_already_current(&mut self) {
        if let Self::Swept {
            already_current, ..
        } = self
        {
            *already_current += 1;
        }
    }
    fn count_failed(&mut self) {
        if let Self::Swept { failed, .. } = self {
            *failed += 1;
        }
    }
}

/// The pending-request `kind` carrying a grant intent. The nest enqueues
/// `kind="subscribe"` for a follow/subscribe intent and `kind="unsubscribe"`
/// for a queued removal (`bins/fauna-nest/src/subscription_handlers.rs` —
/// `insert_subscribe_request`); only the former is a *grant* the tier's
/// `auto_approve` flag governs. An `unsubscribe` row is committed by the drain
/// via [`SubscriptionsAuthor::remove_subscriber`] — no judgment, a leave is
/// not the author's to refuse — and must NEVER reach
/// [`SubscriptionsAuthor::approve_subscriber`] (the approve handler discards
/// the row's `kind` and would re-add the leaver to the roster; the seam
/// refuses it with [`AuthorError::WrongRequestKind`]).
const SUBSCRIBE_KIND: &str = "subscribe";

/// The pending-request `kind` for a subscriber-initiated leave —
/// see [`SUBSCRIBE_KIND`] for the routing rule.
const UNSUBSCRIBE_KIND: &str = "unsubscribe";

/// Max mint+upload attempts before surfacing the last nest rejection. Each
/// attempt re-reads the live roster and advances `rotated_at`, so a transient
/// `roster_mismatch` / `stale_rotation` clears within a couple of tries; the
/// bound stops a misbehaving nest (or pathological skew) from spinning forever.
pub const MAX_MINT_ATTEMPTS: usize = 8;

/// Microseconds added to `rotated_at` on each `stale_rotation` retry. The
/// author can't read the nest's stored `rotated_at` (`key_blob.get` is
/// subscriber-only), so we advance generously instead — only the user's own
/// devices ever write a tier's blobs, so this covers realistic same-fleet clock
/// skew within the attempt bound.
const STALE_BUMP_MICROS: u64 = 1_000_000;

// ── The author pump's cadence policy ────────────────────────────────────────
//
// Every app runs the author reconcile loop, and each owns its own *scheduler*
// (tokio task / Swift `Task` / C# `Task.Delay` / Kotlin coroutine / JS
// `setTimeout`) — that part is genuinely platform-specific and stays per-app
// for the five native/web apps whose runtime this crate cannot name (this
// crate is deliberately wasm-clean — no `tokio` dependency — so it cannot
// spawn or sleep on their behalf; `fauna-ffi`'s `subscriptions_reconcile_once`
// / `subscriptions_author_poll_secs` are their per-tick seam instead). What
// is NOT platform-specific is the *policy*: how long to wait, and what a tick
// does. Both lived in six hand-written copies until 2026-08-01, and one had
// drifted (windows hoisted `resume_pending_removals` out of its loop, so a
// removal staged mid-session healed only at the next connect). A surface that
// re-derives a shared policy will drift; one that is handed it cannot.
//
// linux and tui are different: both already run a bare tokio runtime with no
// FFI/wasm boundary in between, so for THEM the loop itself is not
// platform-specific either — it was two more near-identical hand-written
// copies (differing only in `runtime.spawn` vs ambient `tokio::spawn`) until
// [`run_author_reconcile_loop`] absorbed it below. It stays generic over the
// caller's own sleep fn (never importing `tokio` itself) precisely so this
// crate's wasm-clean contract is untouched by the lift.

/// The author pump's backstop cadence — `monetization.md` § Pillar 1's matrix
/// row names it by value ("`drain_auto_approvals` on connect + **30 s**
/// backstop"). There is no subscribe-request push kind, so this is the delivery
/// floor for a follow that arrives while the author is online.
pub const DEFAULT_AUTHOR_POLL_SECS: u64 = 30;

/// Env override for [`author_poll_secs`], read by the e2e to get a fast drain
/// cadence. Artifact/test wiring, **not** a user-facing knob — the only
/// configuration surface is the apps themselves, so a value a user or admin
/// would ever choose belongs in app UI + nest state, never an env var. This one
/// is neither, hence an env var is correct here.
pub const AUTHOR_POLL_SECS_ENV: &str = "FAUNA_SUBS_POLL_SECS";

/// Pure classifier over a raw override: a positive integer wins, anything else
/// (absent, empty, zero, negative, fractional, unparseable) falls back to
/// [`DEFAULT_AUTHOR_POLL_SECS`]. Zero is rejected on purpose — it would busy-spin
/// the pump against the nest. Kept pure and separate from the env read so the
/// policy is testable without mutating process-global state.
pub fn poll_secs_from_raw(raw: Option<&str>) -> u64 {
    raw.and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_AUTHOR_POLL_SECS)
}

/// The backstop cadence in seconds: [`AUTHOR_POLL_SECS_ENV`] if it carries a
/// usable value, else [`DEFAULT_AUTHOR_POLL_SECS`]. The env probe is native-only
/// — a browser has no environment, so the web SPA gets the constant and supplies
/// its own e2e cadence through the same mechanism its other poll loops use.
pub fn author_poll_secs() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    let raw = std::env::var(AUTHOR_POLL_SECS_ENV).ok();
    #[cfg(target_arch = "wasm32")]
    let raw: Option<String> = None;
    poll_secs_from_raw(raw.as_deref())
}

/// [`author_poll_secs`] as a `Duration`, for shells that sleep directly.
pub fn author_poll_interval() -> core::time::Duration {
    core::time::Duration::from_secs(author_poll_secs())
}

/// **The shared author-pump loop** — for a caller that already runs a bare
/// tokio runtime with no FFI/wasm boundary (today: linux, tui). Ticks
/// [`SubscriptionsAuthor::reconcile_once`] every [`author_poll_interval`],
/// logs each half's failure/success exactly as the two hand-written copies
/// this replaced did, and stops once `generation` no longer holds
/// `my_generation` — the loop's caller bumps `generation` on every fresh
/// login and captures its own post-bump value as `my_generation`, so a newer
/// login (an e2e session driver's repeated re-auth within one process is the
/// case that matters) supersedes and retires this one rather than leaking it
/// to poll a torn-down nest forever.
///
/// Generic over the caller's own `sleep` rather than depending on `tokio`
/// directly, so this crate's wasm-clean contract (no `tokio` in
/// `Cargo.toml` — the web SPA's wasm build relies on it) is untouched by the
/// lift; pass `tokio::time::sleep`. A caller whose scheduler is not tokio
/// (Swift `Task`, C# `Task.Delay`, Kotlin coroutine, JS `setTimeout`) cannot
/// use this — it drives its own loop over the FFI/wasm per-tick calls
/// instead (`fauna-ffi`'s `subscriptions_reconcile_once` /
/// `subscriptions_author_poll_secs`, or `fauna-wasm`'s twins).
pub async fn run_author_reconcile_loop<R, F, Fut>(
    author: &SubscriptionsAuthor<R>,
    generation: &AtomicU64,
    my_generation: u64,
    sleep: F,
) where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    F: Fn(core::time::Duration) -> Fut,
    Fut: core::future::Future<Output = ()>,
{
    let poll = author_poll_interval();
    loop {
        if generation.load(Ordering::SeqCst) != my_generation {
            break;
        }
        let pass = author.reconcile_once().await;
        if let Some(e) = &pass.resume_error {
            tracing::warn!("subscriptions: resume_pending_removals failed: {e}");
        }
        if pass.approved > 0 {
            tracing::info!(
                "subscriptions: auto-approved {} pending follow(s)",
                pass.approved
            );
        }
        if let Some(e) = &pass.drain_error {
            tracing::warn!("subscriptions: drain_auto_approvals failed: {e}");
        }
        sleep(poll).await;
    }
}

/// What one [`SubscriptionsAuthor::reconcile_once`] tick did. Both halves are
/// best-effort and independent: either may fail without costing the other its
/// run, and each reports its own failure as a rendered string so a shell logs it
/// without matching on a generic error type (the shells only ever logged these).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcilePass {
    /// Crash-staged subscriber removals driven to a confirmed nest upload.
    pub resumed: u32,
    /// Queue rows committed by the drain: auto-approved subscribes (a `KeyBlob`
    /// minted for each) + committed unsubscribes (removal rotation + row clear).
    pub approved: u32,
    /// Why the resume half failed, if it did. Rendered, never fatal.
    pub resume_error: Option<String>,
    /// Why the drain half failed, if it did. Rendered, never fatal.
    pub drain_error: Option<String>,
}

/// The **once-per-connect** latch for [`SubscriptionsAuthor::reconcile_once`]'s
/// third half — the stale-keyed-blob pass
/// ([`SubscriptionsAuthor::republish_stale_keyed_blobs`]), which costs one
/// `key_blob.get` per held tier and so does not belong in every 30 s tick.
///
/// Set once a pass completes with no tier failing; a pass that could not
/// finish leaves it clear so the next tick asks again. It lives with the
/// CONNECTION, not the author: linux and tui hold one author for a login's
/// whole loop and the default latch [`SubscriptionsAuthor::new`] builds is
/// already right, but `fauna-ffi` and `fauna-wasm` rebuild the author on every
/// tick, so each keeps one latch on its long-lived client object and hands it
/// in through [`SubscriptionsAuthor::with_connect_pass`] — the seven apps'
/// own pump wiring is untouched.
#[derive(Debug, Clone, Default)]
pub struct ConnectPassLatch(std::sync::Arc<AtomicBool>);

impl ConnectPassLatch {
    fn is_done(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn mark_done(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// What one [`SubscriptionsAuthor::republish_stale_keyed_blobs`] pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StaleKeyedBlobSweep {
    /// Tiers whose live blob wrapped a rotated-out key and now wraps `current`.
    pub republished: usize,
    /// Tiers that could not be read or republished; the next pass retries.
    pub failed: usize,
}

/// A failure from the author-side mint orchestration. Generic over the
/// transport error `E` so it serves native (`NestClientError`) + wasm
/// (`WsRpcError`) without naming either.
#[derive(Debug)]
pub enum AuthorError<E> {
    /// A `fauna.subscriptions.*` WS-RPC call failed at the transport
    /// (disconnect, deadline) or with a non-retryable server rejection.
    Transport(E),
    /// Reading or joining the period-key custody failed — the account store
    /// is not up yet, a row would not decode, or the door refused the write
    /// (no generation tip resolves yet). Never read as "no key held".
    PeriodKeys(StoreError),
    /// The tier has no client-held period key — it was never created in
    /// encrypted mode (so the author can't mint). Carries the tier name.
    NoPeriodKey(String),
    /// Assembling the `ManageSubscribers` self-delegation failed.
    SelfDelegation(SelfDelegationError),
    /// Minting / signing the `KeyBlob` failed.
    Mint(MintError),
    /// A custody transition (commit-period) failed.
    Custody(CustodyError),
    /// A `KeyBlob` the nest served back could not be decoded. Read as a hard
    /// failure for that tier rather than "nothing owed": the succession
    /// rotation asks the stored blob whether its own rotation's upload landed,
    /// so a blob it cannot read is an answer it does not have, and guessing in
    /// the safe-looking direction would retire the exposure on a parse fault.
    BlobDecode(String),
    /// The nest kept rejecting the mint past [`MAX_MINT_ATTEMPTS`] (the roster /
    /// rotation kept shifting). Carries the last retryable wire code.
    RetriesExhausted { code: String },
    /// A pending request of the wrong `kind` reached a kind-specific commit
    /// path (an `unsubscribe` row fed to `approve_subscriber` would re-add the
    /// leaver). Carries the offending kind.
    WrongRequestKind { kind: String },
    /// A pending request names a tier the author's own `tiers.list` reports
    /// `hidden` (`monetization.md` § The unifying model → *A tier may be
    /// hidden*, ruling 4: not offered and not subscribable) — no approve-mint
    /// may cover it, whatever enqueued it. Carries the tier name.
    HiddenTierRequest { tier: String },
    /// The nest created the reserved tier **offered** — the `hidden`
    /// flag did not take and the tier is on every
    /// offer surface. The fail-closed backstop for a nest that advertises
    /// `hidden-tiers` and still answers `hidden: false`: never gate content to
    /// a tier strangers can subscribe to.
    HiddenTierNotHonored,
    /// After a reserved-name `tiers.create` carrying this client's birth blob,
    /// the nest's live blob is NOT the one uploaded — a sibling device won the
    /// race, or the nest kept a blob the read before the create missed. The
    /// custody key and the live blob then name different keys; gate material
    /// made of the pair would seal posts no follower could ever open, so it is
    /// refused — here, and on every later call while the live blob's key
    /// witness names a key other than custody's `current`.
    LiveBlobMismatch { tier: String },
    /// An approve met `stale_rotation` while a removal's rotation of the same
    /// tier is staged but not yet committed: the live blob may already wrap
    /// the staged key, which custody does not call `current` yet. The approve
    /// can neither bump past it nor adopt the staged key, so it stands down —
    /// transient, the request stays queued and the next pump pass retries it
    /// once the removal has committed. Carries the tier name.
    RotationInFlight { tier: String },
}

/// The nest's own "no key blob for this tier" answer
/// (`subscription_handlers::key_blob_not_found`) — the ONE `key_blob.get`
/// failure that means "no blob"; every other failure is a fault.
const KEY_BLOB_NOT_FOUND: &str = "fauna.subscriptions.key_blob_not_found";

impl<E: core::fmt::Display> core::fmt::Display for AuthorError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "subscriptions transport: {e}"),
            // The custody is the account plane's: say
            // read/write in the seam's own words, never the shared
            // `StoreError`'s Display.
            Self::PeriodKeys(StoreError::Load(m)) => {
                write!(f, "subscriptions period keys read: {m}")
            }
            Self::PeriodKeys(StoreError::Save(m)) => {
                write!(f, "subscriptions period keys write: {m}")
            }
            Self::NoPeriodKey(t) => {
                write!(
                    f,
                    "no client-held period key for tier {t:?} (not created in encrypted mode)"
                )
            }
            Self::SelfDelegation(e) => write!(f, "subscriptions self-delegation: {e}"),
            Self::Mint(e) => write!(f, "subscriptions mint: {e}"),
            Self::Custody(e) => write!(f, "subscriptions custody: {e}"),
            Self::BlobDecode(e) => write!(f, "subscriptions key blob decode: {e}"),
            Self::RetriesExhausted { code } => {
                write!(
                    f,
                    "subscriptions mint retries exhausted (last nest code: {code})"
                )
            }
            Self::WrongRequestKind { kind } => {
                write!(
                    f,
                    "request kind {kind:?} cannot be approve-minted (an unsubscribe \
                     commits via the removal rotation)"
                )
            }
            Self::HiddenTierRequest { tier } => {
                write!(
                    f,
                    "tier {tier:?} is hidden — never subscribable, so no request \
                     against it may be approve-minted"
                )
            }
            Self::HiddenTierNotHonored => {
                write!(
                    f,
                    "the nest created the reserved tier offered — it did not honour the hidden flag"
                )
            }
            Self::LiveBlobMismatch { tier } => {
                write!(
                    f,
                    "the nest's live key blob for tier {tier:?} is not the one this client \
                     uploaded — custody and nest name different keys"
                )
            }
            Self::RotationInFlight { tier } => {
                write!(
                    f,
                    "a removal's key rotation of tier {tier:?} is staged but not yet \
                     committed — the approve stands down until it is"
                )
            }
        }
    }
}

impl<E: core::fmt::Display + core::fmt::Debug> std::error::Error for AuthorError<E> {}

/// The encrypted-mode author-side subscription orchestration. Bundles the thin
/// `fauna.subscriptions.*` call surface ([`SubscriptionsClient`]) with the
/// owner-private state it mutates ([`crate::period_keys::PeriodKeyStore`] — the period-key
/// custody + crash-recovery sentinel) and the identity it mints as (the
/// keypair the mint signs with, whose actor id every minted period is stamped
/// with). One instance per actor on
/// each Fauna app; cheap to hold.
pub struct SubscriptionsAuthor<R: RpcRequester> {
    subs: SubscriptionsClient<R>,
    keypair: ActorKeypair,
    period_keys: SharedPeriodKeyStore,
    connect_pass: ConnectPassLatch,
}

/// A tier whose period key is minted + persisted and whose birth `KeyBlob` is
/// built, but which does not exist server-side yet — the output of
/// [`SubscriptionsAuthor::stage_tier`], consumed by
/// [`SubscriptionsAuthor::commit_tier`].
///
/// Exists for the "sell this post" ordering (`monetization.md` § Per-post
/// pay-to-unlock): the gated post body is sealed under [`Self::period_key`] and
/// names [`Self::key_blob_ref`], and only the resulting body's hash — the
/// `post_id` — can be handed to `tiers.create` as the designation. Both fields
/// are therefore needed *before* the tier exists.
pub struct StagedTier {
    /// The persisted period key the gated body is sealed under. Sealing under
    /// anything else would make the post unreadable by every buyer.
    pub period_key: [u8; 32],
    /// The birth blob's content address, derived exactly as the nest derives
    /// it. This is the post's `key_access: Broadcast { key_blob_ref }`.
    pub key_blob_ref: [u8; 32],
    /// The minted birth blob itself. Must be the blob that is committed — a
    /// re-mint stamps a new timestamp and hashes differently.
    pub upload: EncryptedKeyBlobUpload,
}

/// The material a gated post seals under, for one tier — what
/// `FeedManager::prepare_gated_blob` assembles by hand today (period key from
/// custody, the live blob's address from `key_blob.get`). Returned by the two
/// provisioning methods so the archive-import machine gates a post exactly the
/// way the compose leg does.
pub struct TierGateMaterial {
    /// The tier the post is gated to.
    pub tier: String,
    /// The tier's rank — the `RestrictedPostAudience` cascade bound.
    pub rank: u32,
    /// The active period's key (Pillar-1 custody).
    pub period_key: Zeroizing<[u8; 32]>,
    /// The active period's version (`TierPeriod::version`) — the
    /// `RestrictedPostAudience::Period::period_epoch` a sealed media item names.
    pub period_version: u64,
    /// The live `KeyBlob`'s content address (`key_blob.get` → `blob_hash`).
    pub key_blob_ref: [u8; 32],
}

impl<R: RpcRequester> SubscriptionsAuthor<R> {
    /// Build over the thin subscription client, the identity (the keypair the
    /// mint signs the `KeyBlob` + self-delegation with) and the period-key
    /// custody store (the account runtime's handle; a store that is not up yet
    /// answers every custody read with an error, never an empty custody).
    pub fn new(
        subs: SubscriptionsClient<R>,
        keypair: ActorKeypair,
        period_keys: SharedPeriodKeyStore,
    ) -> Self {
        Self {
            subs,
            keypair,
            period_keys,
            connect_pass: ConnectPassLatch::default(),
        }
    }

    /// [`Self::new`] over one transport, as `keypair` — which is how every host
    /// builds it.
    pub fn over(nest: R, keypair: ActorKeypair, period_keys: SharedPeriodKeyStore) -> Self {
        Self::new(SubscriptionsClient::new(nest), keypair, period_keys)
    }

    /// The account's period-key custody, folded.
    async fn custody(
        &self,
    ) -> Result<fauna_core::data::SubscriptionsConfig, AuthorError<R::Error>> {
        self.period_keys
            .custody()
            .await
            .map_err(AuthorError::PeriodKeys)
    }

    /// [`Self::custody`] for a read-join-put — the read a user's write gesture
    /// starts from, which waits out a runtime still assembling as the join
    /// does ([`PeriodKeyStore::custody_for_write`]).
    async fn custody_for_write(
        &self,
    ) -> Result<fauna_core::data::SubscriptionsConfig, AuthorError<R::Error>> {
        self.period_keys
            .custody_for_write()
            .await
            .map_err(AuthorError::PeriodKeys)
    }

    /// Join `replica` into the custody store; the custody as it now stands.
    async fn merge_custody(
        &self,
        replica: fauna_core::data::SubscriptionsConfig,
    ) -> Result<fauna_core::data::SubscriptionsConfig, AuthorError<R::Error>> {
        self.period_keys
            .merge_custody(replica)
            .await
            .map_err(AuthorError::PeriodKeys)
    }

    /// Record a fresh v1 period for `tier` unless one is held (the idempotent
    /// [`custody::record_new_tier`]), persisted before anything leaves; the
    /// tier's `current` as the store now holds it — a peer device's
    /// concurrently recorded key wins here exactly as it wins every later read.
    async fn record_tier_period(&self, tier: &str) -> Result<TierPeriod, AuthorError<R::Error>> {
        let mut replica = self.custody_for_write().await?;
        if custody::current_period(&replica, tier).is_none() {
            custody::record_new_tier(
                &mut replica,
                self.keypair.actor_id(),
                tier,
                *fresh_period_key(),
                Timestamp::now().0,
            );
            replica = self.merge_custody(replica).await?;
        }
        custody::current_period(&replica, tier)
            .ok_or_else(|| AuthorError::NoPeriodKey(tier.to_string()))
    }

    /// Share the connection's [`ConnectPassLatch`] — for a caller that builds
    /// a fresh author per pump tick, so the once-per-connect pass runs once per
    /// connection rather than on every tick.
    pub fn with_connect_pass(mut self, latch: ConnectPassLatch) -> Self {
        self.connect_pass = latch;
        self
    }

    /// Borrow the thin subscription client for the pure reads the UI renders
    /// from (`requests_list`, `subscribers_list`, the tier/status getters) —
    /// those need no orchestration.
    pub fn client(&self) -> &SubscriptionsClient<R> {
        &self.subs
    }

    /// Mint + sign the `encrypted_upload` envelope: a self-signed
    /// `ManageSubscribers` `DeviceAuthorization` plus the `KeyBlob` wrapping
    /// `period_key` to `subscribers`, both in the embed-as-bytes wire shape.
    ///
    /// `subscribers` carries each member's published ML-KEM ek (or `None`).
    /// When a member published one, that member's `KeyBlobEntry` is X-Wing (surface B, S4b) —
    /// otherwise classical, so a mixed roster mints per-entry (the
    /// `mint_key_blob` selector degrades non-erroringly). A stored ek of the
    /// wrong length silently degrades that member to classical.
    fn mint_upload(
        &self,
        tier: &str,
        rotated_at: u64,
        subscribers: &[(ActorId, Option<Vec<u8>>)],
        period_key: &[u8; 32],
    ) -> Result<EncryptedKeyBlobUpload, AuthorError<R::Error>> {
        // Minting for subscribers is a seed-holding author's operation (R4 (account-data-plane.md § The ratified decisions)), so
        // a seedless client (the app-dead sync agent) has nothing to sign with.
        let keypair = &self.keypair;
        mint_self_delegated_upload(keypair, tier, rotated_at, subscribers, period_key).map_err(
            |e| match e {
                MintUploadError::SelfDelegation(e) => AuthorError::SelfDelegation(e),
                MintUploadError::Mint(e) => AuthorError::Mint(e),
            },
        )
    }
}

/// Why [`mint_self_delegated_upload`] could not build an upload.
#[derive(Debug)]
pub enum MintUploadError {
    /// Assembling the `ManageSubscribers` self-delegation failed.
    SelfDelegation(SelfDelegationError),
    /// Minting / signing the `KeyBlob` failed.
    Mint(MintError),
}

impl std::fmt::Display for MintUploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SelfDelegation(e) => write!(f, "self-delegation: {e}"),
            Self::Mint(e) => write!(f, "mint key blob: {e}"),
        }
    }
}

/// Mint a `KeyBlob` for `tier` as the seed-holding author `keypair` — signed
/// under a fresh `ManageSubscribers` self-delegation — and package it as the
/// `EncryptedKeyBlobUpload` every upload-carrying kind (`tiers.create`,
/// `requests.approve`, the rotations) takes.
///
/// The one place an author-side upload is assembled: the orchestration's own
/// mints, and the e2e harness's C-ABI birth-blob builder, all come through
/// here, so a fixture can never agree with itself about an envelope the
/// nest would refuse.
pub fn mint_self_delegated_upload(
    keypair: &ActorKeypair,
    tier: &str,
    rotated_at: u64,
    subscribers: &[(ActorId, Option<Vec<u8>>)],
    period_key: &[u8; 32],
) -> Result<EncryptedKeyBlobUpload, MintUploadError> {
    let signed = build_manage_subscribers_self_delegation(keypair, Timestamp::now())
        .map_err(MintUploadError::SelfDelegation)?;
    let ids: Vec<ActorId> = subscribers.iter().map(|(id, _)| *id).collect();
    let eks: Vec<Option<[u8; MLKEM768_ENCAPS_KEY_LEN]>> = subscribers
        .iter()
        .map(|(_, ek)| ek.as_deref().and_then(|b| b.try_into().ok()))
        .collect();
    let minted = mint_key_blob(
        keypair,
        &signed.auth,
        &signed.bytes,
        &signed.envelope,
        tier.to_string(),
        Timestamp(rotated_at),
        &ids,
        &eks,
        period_key,
    )
    .map_err(MintUploadError::Mint)?;
    Ok(EncryptedKeyBlobUpload {
        key_blob: EmbedAsBytes::from_signed(minted.bytes, minted.envelope),
        signer_auth: signed.wire(),
        extra: Default::default(),
    })
}

impl<R: RpcRequester> SubscriptionsAuthor<R>
where
    R::Error: RpcErrorClass,
{
    /// Create a subscription tier (encrypted mode): record a fresh period key in
    /// custody, persist it, then create the tier server-side.
    ///
    /// **Custody-first + persist-first** so a crash after the server create but
    /// before the key persists cannot strand a tier the author can't mint for:
    /// `record_new_tier` is idempotent (a retry keeps the existing key), and no
    /// blob is minted at create (empty roster), so a never-used fresh key is
    /// harmless. Returns whether the server created a new row (`false` =
    /// idempotent repeat).
    ///
    /// `unlocks_post` makes this a **per-post pay-to-unlock** tier naming the
    /// hex `post_id` it sells (`monetization.md` § Per-post pay-to-unlock) —
    /// the "sell this post" flow mints the tier this way and then gates that
    /// post to it. `None` for every ordinary tier. The designation is
    /// create-time immutable, so this is the only place it can ever be set.
    ///
    /// `asking_price` names the tier's machine-comparable threshold
    /// (`monetization.md` § The asking price). `None` for a tier no inferring
    /// mechanism may buy — the default everywhere, and the only correct
    /// answer for a tier whose author has expressed no machine price.
    ///
    /// `hidden` withholds the tier from every offer surface and refuses
    /// `subscribe` (`monetization.md` § The unifying model → *A tier may be
    /// hidden*); `false` on every ordinary tier.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_tier(
        &self,
        name: &str,
        rank: u32,
        description: Option<String>,
        price_hint: Option<String>,
        payment_url: Option<String>,
        auto_approve: bool,
        unlocks_post: Option<String>,
        asking_price: Option<TierAskingPrice>,
        hidden: bool,
    ) -> Result<bool, AuthorError<R::Error>> {
        let staged = self.stage_tier(name).await?;
        self.commit_tier(
            name,
            rank,
            description,
            price_hint,
            payment_url,
            auto_approve,
            staged,
            unlocks_post,
            asking_price,
            hidden,
        )
        .await
    }

    /// Phase 1 of tier creation: mint + persist the period key and build the
    /// birth `KeyBlob` **locally**, touching no server state.
    ///
    /// Split out of [`create_tier`](Self::create_tier) for the "sell this post"
    /// flow (`monetization.md` § Per-post pay-to-unlock), whose ordering is
    /// circular unless this phase is observable: the gated post body must name
    /// this tier *and* its birth blob's content address, and the tier can only
    /// be created once `post_id = blake3(body)` exists. So the caller stages,
    /// builds the body against [`StagedTier`], then
    /// [`commit_tier`](Self::commit_tier)s with the designation.
    ///
    /// The staged blob must be the one committed: [`mint_upload`] stamps
    /// `Timestamp::now()`, so re-minting yields a *different* content address
    /// than the one already signed into the post body.
    pub async fn stage_tier(&self, name: &str) -> Result<StagedTier, AuthorError<R::Error>> {
        // Persist the fresh (irrecoverable) period key before anything leaves:
        // a join, so a peer device's concurrent write (another tier, a staged
        // removal) is never dropped, and a retry after a crash (or an
        // idempotent repeat) mints the birth blob under the SAME persisted
        // key, not a fresh throwaway.
        let period = self.record_tier_period(name).await?;
        // Birth KeyBlob: the empty-roster blob under the fresh period key, so
        // the tier has a live `KeyBlob` from creation (`ui/feed.md`
        // § Encryption at rest — broadcast tiers) and gate-to-tier compose
        // works before the first subscriber.
        let upload = self.mint_upload(name, period.rotated_at, &[], &period.key)?;
        // The nest stores `blake3(upload.key_blob.bytes)` as the blob's content
        // address (`subscription_handlers.rs` — the `key_blob.get` reply the
        // ordinary compose path reads back). Deriving it here is what lets the
        // body be built before the tier exists.
        let key_blob_ref = fauna_core::encoding::content_hash(&upload.key_blob.bytes).digest();
        Ok(StagedTier {
            period_key: period.key.clone().into(),
            key_blob_ref,
            upload,
        })
    }

    /// Phase 2 of tier creation: create the tier server-side, carrying the blob
    /// staged by [`stage_tier`](Self::stage_tier), — for a per-post unlock
    /// tier — the create-time-immutable `unlocks_post` designation, and the
    /// optional machine-comparable `asking_price`.
    ///
    /// The two designations travel together here on purpose: a sold post's
    /// tier is precisely the one an inferring mechanism needs a price for
    /// (`monetization.md` § Per-post pay-to-unlock — "a zap receipt targeting
    /// the post … compares the receipt's msat amount against that tier's
    /// asking price"), so minting the tier and naming its price is one act.
    /// Passing `None` leaves the tier unbuyable by inference, which is the
    /// correct default and stays valid forever.
    ///
    /// `hidden` withholds the tier from every offer surface and refuses
    /// `subscribe` (`monetization.md` § The unifying model → *A tier may be
    /// hidden*); `false` on every ordinary tier.
    #[allow(clippy::too_many_arguments)]
    pub async fn commit_tier(
        &self,
        name: &str,
        rank: u32,
        description: Option<String>,
        price_hint: Option<String>,
        payment_url: Option<String>,
        auto_approve: bool,
        staged: StagedTier,
        unlocks_post: Option<String>,
        asking_price: Option<TierAskingPrice>,
        hidden: bool,
    ) -> Result<bool, AuthorError<R::Error>> {
        self.subs
            .tiers_create(
                name,
                rank,
                description,
                price_hint,
                payment_url,
                auto_approve,
                staged.upload,
                unlocks_post,
                asking_price,
                hidden,
            )
            .await
            .map_err(AuthorError::Transport)
    }

    /// Read the live blob's address for `tier` (`key_blob.get` → `blob_hash`).
    async fn live_key_blob_ref(&self, tier: &str) -> Result<[u8; 32], AuthorError<R::Error>> {
        let me = self.keypair.actor_id();
        let reply = self
            .subs
            .key_blob_get(me, tier)
            .await
            .map_err(AuthorError::Transport)?;
        reply
            .blob_hash
            .as_ref()
            .try_into()
            .map_err(|_| AuthorError::NoPeriodKey(tier.to_string()))
    }

    /// [`Self::live_key_blob_ref`] with exactly one answer read as "no blob":
    /// the nest's own [`KEY_BLOB_NOT_FOUND`]. Every other failure — a dropped
    /// connection, a deadline, an auth refusal — stays an error, because
    /// reading a transport fault as "no blob" is how custody ends up holding a
    /// fresh key beside a live blob that wraps another
    /// ([`Self::provision_followers_tier`]).
    async fn live_key_blob_ref_if_any(
        &self,
        tier: &str,
    ) -> Result<Option<[u8; 32]>, AuthorError<R::Error>> {
        match self.live_key_blob_ref(tier).await {
            Ok(r) => Ok(Some(r)),
            Err(AuthorError::Transport(e))
                if retryable_code(&e).as_deref() == Some(KEY_BLOB_NOT_FOUND) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Ensure the reserved **owner-only** tier exists hidden, unapproved, at the
    /// top rank, and return its gate material (`archive-import.md` § Audience
    /// mapping; `monetization.md` § The unifying model → *A tier may be
    /// hidden*). Idempotent: an existing row on the author's own `tiers.list`
    /// is reused; a row the nest reports OFFERED (`hidden: false` — a nest that
    /// stored the flag in `extra`) is refused with
    /// [`AuthorError::HiddenTierNotHonored`], never gated to.
    pub async fn provision_owner_only_tier(
        &self,
    ) -> Result<TierGateMaterial, AuthorError<R::Error>> {
        let tiers = self
            .subs
            .tiers_list()
            .await
            .map_err(AuthorError::Transport)?;
        match tiers.iter().find(|t| t.name == OWNER_ONLY_TIER) {
            Some(existing) if !existing.hidden => return Err(AuthorError::HiddenTierNotHonored),
            Some(_) => {}
            None => {
                let staged = self.stage_tier(OWNER_ONLY_TIER).await?;
                self.commit_tier(
                    OWNER_ONLY_TIER,
                    OWNER_ONLY_TIER_RANK,
                    None,
                    None,
                    None,
                    false, // never auto-approve: nothing may ever be granted
                    staged,
                    None,
                    None,
                    true, // hidden
                )
                .await?;
                let tiers = self
                    .subs
                    .tiers_list()
                    .await
                    .map_err(AuthorError::Transport)?;
                if !tiers.iter().any(|t| t.name == OWNER_ONLY_TIER && t.hidden) {
                    return Err(AuthorError::HiddenTierNotHonored);
                }
            }
        }
        let period = custody::current_period(&self.custody().await?, OWNER_ONLY_TIER)
            .ok_or_else(|| AuthorError::NoPeriodKey(OWNER_ONLY_TIER.to_string()))?;
        Ok(TierGateMaterial {
            tier: OWNER_ONLY_TIER.to_string(),
            rank: OWNER_ONLY_TIER_RANK,
            period_key: Zeroizing::new(period.key.clone().into()),
            period_version: period.version,
            key_blob_ref: self.live_key_blob_ref(OWNER_ONLY_TIER).await?,
        })
    }

    /// Ensure the reserved **followers** tier is gate-able — a period key in
    /// custody *and* a live `KeyBlob` on the nest that wraps **that** key — for
    /// an account nobody follows yet, through the slice-3 `tiers.create`
    /// relaxation (`monetization.md` § The unifying model, the rank-0
    /// paragraph).
    ///
    /// The nest's blob is read FIRST, because the two halves must agree: gate
    /// material pairing a period key with a blob that wraps a *different* key
    /// produces posts no follower can ever read, and nothing downstream would
    /// notice. The three reachable states:
    ///
    /// 1. **custody has the key, the nest has a blob** — reuse both, mint
    ///    nothing (the idempotent repeat).
    /// 2. **custody has the key, the nest has no blob** — mint the birth blob
    ///    under the custody key and create the reserved row.
    /// 3. **custody is empty, the nest has no blob** — record a fresh key (the
    ///    approve-time arm's `record_new_tier` shape), then as (2). This is the
    ///    follower-less author the relaxation exists for.
    ///
    /// The fourth combination — **custody empty while the nest holds a blob** —
    /// is refused with [`AuthorError::NoPeriodKey`] and leaves custody
    /// untouched. It means the author's period key was lost, and this method
    /// may not paper over that: recording a fresh key here would strand every
    /// post already gated to followers under the old one, on top of producing
    /// material that mismatches the live blob. The archive-import machine
    /// skip-logs `Friends` content with that reason instead.
    ///
    /// Three guards keep the two halves honest. The nest's blob is read
    /// **precisely** — only its own `key_blob_not_found` is "no blob"; a
    /// transport fault propagates instead of being read as (3), which would
    /// record a fresh key beside a live blob that wraps another. After a
    /// create that carried this client's birth blob, the live blob's address is
    /// read back and must be the uploaded blob's: the nest keeps a blob it
    /// already held and answers `created: false` with no other signal, so a
    /// sibling device winning the race would otherwise leave custody naming one
    /// key and the live blob another — refused as
    /// [`AuthorError::LiveBlobMismatch`]. The period this call recorded stays
    /// (custody never deletes a key; nothing was sealed under it), so state
    /// (1) checks the pair itself: the live blob's key witness
    /// (`KeyBlob::key_commitment`, read by [`custody::live_blob_key`]) must
    /// name custody's `current` — a blob wrapping any other key is the same
    /// [`AuthorError::LiveBlobMismatch`], never gate material, until the
    /// sibling's key arrives by walk (it then wins or loses `current` by the
    /// merge's order, and a rotated-out live blob is the reconcile pass's
    /// republish).
    pub async fn provision_followers_tier(
        &self,
    ) -> Result<TierGateMaterial, AuthorError<R::Error>> {
        let live = self.live_key_blob_ref_if_any(FOLLOWERS_TIER).await?;

        let custody_now = self.custody().await?;
        let period = match (custody::current_period(&custody_now, FOLLOWERS_TIER), live) {
            // (1): custody and the nest both hold one — they must name ONE key.
            (Some(p), Some(_)) => {
                let me = self.keypair.actor_id();
                if let Some(stored) = self.stored_key_blob(me, FOLLOWERS_TIER).await? {
                    let key = custody::live_blob_key(
                        &custody_now,
                        FOLLOWERS_TIER,
                        &stored.key_commitment,
                    );
                    if key != Some(custody::LiveBlobKey::Current) {
                        return Err(AuthorError::LiveBlobMismatch {
                            tier: FOLLOWERS_TIER.to_string(),
                        });
                    }
                }
                p
            }
            // (2): custody decides the key; the blob arm below mints.
            (Some(p), None) => p,
            // (4): the live blob wraps a key this client no longer holds.
            (None, Some(_)) => return Err(AuthorError::NoPeriodKey(FOLLOWERS_TIER.to_string())),
            // (3): nothing exists yet — the follower-less author. Recorded
            // BEFORE the upload leaves, the crash-safe shape every minter here
            // takes: a key that exists only in memory while its blob is on the
            // wire is lost with the process.
            (None, None) => self.record_tier_period(FOLLOWERS_TIER).await?,
        };

        let rank = u32::try_from(FOLLOWERS_TIER_RANK).expect("the reserved rank fits u32");
        let key_blob_ref = match live {
            Some(r) => r,
            None => {
                let upload =
                    self.mint_upload(FOLLOWERS_TIER, period.rotated_at, &[], &period.key)?;
                // The address the nest stores for THIS blob — the content hash
                // of the signed inner bytes, exactly what
                // `tiers_create_handler`'s reserved arm (and every other
                // `upsert_current_key_blob` writer) hashes — taken before the
                // upload leaves, so the read-back below has something to agree
                // with.
                let minted = fauna_core::encoding::content_hash(&upload.key_blob.bytes).digest();
                self.subs
                    .tiers_create(
                        FOLLOWERS_TIER,
                        rank,
                        None,
                        None,
                        None,
                        true,
                        upload,
                        None,
                        None,
                        false,
                    )
                    .await
                    .map_err(AuthorError::Transport)?;
                let stored = self.live_key_blob_ref(FOLLOWERS_TIER).await?;
                if stored != minted {
                    return Err(AuthorError::LiveBlobMismatch {
                        tier: FOLLOWERS_TIER.to_string(),
                    });
                }
                stored
            }
        };
        Ok(TierGateMaterial {
            tier: FOLLOWERS_TIER.to_string(),
            rank,
            period_key: Zeroizing::new(period.key.clone().into()),
            period_version: period.version,
            key_blob_ref,
        })
    }

    /// Approve a pending subscribe request: mint a `KeyBlob` over the
    /// post-approval roster (current subscribers ∪ the requester) under the
    /// tier's **current** period key (no rotation), and upload it via
    /// `requests.approve`. Retries `roster_mismatch` / `stale_rotation` with a
    /// re-read roster + advanced `rotated_at`; a `stale_rotation` also re-reads
    /// custody and mints under whatever period is current then, or stands down
    /// ([`AuthorError::RotationInFlight`]) while a removal's rotation of the
    /// tier is staged. Adding a member to an existing
    /// tier needs no custody mutation; the one exception is the reserved
    /// `followers` tier on its *first* approval — see below.
    pub async fn approve_subscriber(
        &self,
        request: &PendingRequest,
    ) -> Result<ApproveRequestReply, AuthorError<R::Error>> {
        // The nest's approve handler discards the row's `kind`, so approving an
        // `unsubscribe` row would mint a KeyBlob still covering the leaver and
        // silently cancel their leave — refuse the misroute at the seam
        // (an unsubscribe commits via `remove_subscriber`; the drain does this).
        if request.kind != SUBSCRIBE_KIND {
            return Err(AuthorError::WrongRequestKind {
                kind: request.kind.clone(),
            });
        }
        // Ruling 4 (`monetization.md` § The unifying model → *A tier may be
        // hidden*): hidden means not offered and not subscribable, so no
        // approve-mint may cover it — whatever enqueued the request (a
        // `payment_entitled` signal bypasses `auto_approve`, but never this).
        // Checked here independently of the drain's own skip
        // ([`Self::drain_auto_approvals`]): the two back each other up, so
        // either alone still refuses a direct [`Self::approve_subscriber`]
        // call outside the pump.
        let tiers = self
            .subs
            .tiers_list()
            .await
            .map_err(AuthorError::Transport)?;
        if tiers
            .iter()
            .any(|t| t.name == request.tier_name && t.hidden)
        {
            return Err(AuthorError::HiddenTierRequest {
                tier: request.tier_name.clone(),
            });
        }
        let mut period = match custody::current_period(&self.custody().await?, &request.tier_name) {
            Some(p) => p,
            // The nest auto-provisions the reserved free `followers` tier (follow
            // = subscribe to it), so the author's client never ran `create_tier`
            // for it and holds no period key. Generate + persist a v1 key on the
            // first follow-approval — the same join-safe, idempotent shape as
            // `create_tier` (a concurrent peer device converges via the join
            // and `record_new_tier`'s no-op-if-present). Scoped to the
            // reserved tier: any *other* missing period key is a real bug (a
            // dropped `create_tier` custody write), not auto-healed.
            None if request.tier_name == FOLLOWERS_TIER => {
                self.record_tier_period(&request.tier_name).await?
            }
            None => return Err(AuthorError::NoPeriodKey(request.tier_name.clone())),
        };

        // The brand-new subscriber isn't on the roster yet, so their published ek
        // rides the request (S4b: requests.list surfaces it).
        let new_ek = request.mlkem_encaps_key.as_ref().map(|b| b.to_vec());

        let mut rotated_at = next_rotated_at(period.rotated_at);
        let mut last_code = String::new();
        for _ in 0..MAX_MINT_ATTEMPTS {
            let mut subscribers = self.roster(&request.tier_name).await?;
            if !subscribers
                .iter()
                .any(|(id, _)| *id == request.subscriber_id)
            {
                subscribers.push((request.subscriber_id, new_ek.clone()));
            }
            let upload =
                self.mint_upload(&request.tier_name, rotated_at, &subscribers, &period.key)?;
            match self
                .subs
                .requests_approve(request.request_id, Some(upload))
                .await
            {
                Ok(reply) => return Ok(reply),
                Err(e) => match retryable_code(&e).as_deref() {
                    Some(ROSTER_MISMATCH) => {
                        rotated_at = next_rotated_at(rotated_at);
                        last_code = ROSTER_MISMATCH.into();
                    }
                    // Someone published past us — and that someone may have
                    // ROTATED. Never bump past a publisher this approve has
                    // not read: re-read custody first.
                    // Every rotation persists its key before its upload (the
                    // leg into `current`, a removal into `pending_removals`),
                    // so a blob live enough to refuse us is visible here.
                    Some(STALE_ROTATION) => {
                        let custody_now = self.custody().await?;
                        if custody_now
                            .pending_removals
                            .iter()
                            .any(|r| r.tier_name == request.tier_name)
                        {
                            return Err(AuthorError::RotationInFlight {
                                tier: request.tier_name.clone(),
                            });
                        }
                        let now = custody::current_period(&custody_now, &request.tier_name)
                            .ok_or_else(|| AuthorError::NoPeriodKey(request.tier_name.clone()))?;
                        rotated_at = (next_rotated_at(rotated_at) + STALE_BUMP_MICROS)
                            .max(next_rotated_at(now.rotated_at));
                        period = now;
                        last_code = STALE_ROTATION.into();
                    }
                    _ => return Err(AuthorError::Transport(e)),
                },
            }
        }
        Err(AuthorError::RetriesExhausted { code: last_code })
    }

    /// Auto-approve every pending **subscribe** request whose tier is
    /// `auto_approve`, minting the covering `KeyBlob` for each (via
    /// [`Self::approve_subscriber`]). Returns the count approved this pass.
    ///
    /// This is the encrypted-mode realization of the `auto_approve` contract:
    /// `auto_approve = true` means "grant on subscribe with **no creator
    /// action**" (`monetization.md` § The unifying model — entitlement grant
    /// path 2), but in encrypted mode the nest holds no period key and so
    /// *cannot* mint — the subscribe therefore **enqueues** (`Queued`) and the
    /// author's client must pick it up and mint (§ Pillar 1 — "the creator's
    /// client picks them up, mints a roster-covering `KeyBlob`, and uploads
    /// it"). Run this on client connect (and on a subscribe-request push if the
    /// transport has one, with a poll backstop — the `start_receive_loop`
    /// shape) so a queued request transitions `Queued` → granted once the
    /// author is online, with no manual approval. The canonical case is the
    /// free rank-0 `followers` tier (follow = subscribe to it), so this is what
    /// makes an encrypted-mode **follow** frictionless.
    ///
    /// **Never a `hidden` tier, whatever else is true of the request.**
    /// Ruling 4 (`monetization.md` § The unifying model → *A tier may be
    /// hidden*) makes a hidden tier not subscribable at all, so a
    /// `payment_entitled` or `auto_approve` request naming one is skipped,
    /// never minted — the residue of a request that reached this queue past
    /// an enqueue-side gap (a future regression there).
    ///
    /// **Only `auto_approve` tiers or `payment_entitled` requests, only
    /// `subscribe` requests.** A non-auto tier's request stays pending for
    /// the author to approve by hand (the § Pillar 1 "Pending requests" UI)
    /// — unless a configured payment provider verified the subscriber paid
    /// for it (`PendingRequest::payment_entitled`, monetization.md
    /// § Pillar 3): a verified payment is the third grant source and needs
    /// no creator judgment, so the pump mints for it exactly like an
    /// auto-approval. An `unsubscribe` row is COMMITTED unconditionally —
    /// a leave is not the author's to refuse — via [`Self::remove_subscriber`]
    /// plus a queue-row delete, and unsubscribes run BEFORE the approve-mints
    /// so no same-pass mint covers a leaver (see [`SUBSCRIBE_KIND`] for why an
    /// unsubscribe row must never reach `approve_subscriber`). `requests.list`
    /// carries no `auto_approve` discriminant (only `tier_name` + `kind`), so
    /// the auto-approve set is resolved from the author's own `tiers.list`
    /// (which carries `auto_approve`, including the auto-provisioned `followers`
    /// row — `bins/fauna-nest/src/subscription_handlers.rs::ensure_followers_tier`
    /// sets `auto_approve = true`).
    ///
    /// **Best-effort + idempotent.** A single poisoned request — a paid tier
    /// whose custody period key was dropped (surfacing
    /// [`AuthorError::NoPeriodKey`]), or a persistent roster fight
    /// ([`AuthorError::RetriesExhausted`]) — is skipped so it can't starve the
    /// other follows; the first error is surfaced only when **nothing** drained
    /// (so a mid-drain disconnect, or a lone-poison steady state, stays visible
    /// to the caller to log), while a partial success returns the count and
    /// lets the next connect/notification pass retry the stragglers. The nest
    /// deletes each approved request row (`encrypted_approve`), so a re-run
    /// never re-approves an already-granted subscriber.
    pub async fn drain_auto_approvals(&self) -> Result<usize, AuthorError<R::Error>> {
        let requests = self
            .subs
            .requests_list()
            .await
            .map_err(AuthorError::Transport)?;
        if requests.is_empty() {
            return Ok(0);
        }
        // `requests.list` carries no `auto_approve`, so resolve the auto-approve
        // tier set from the author's own definitions (`tiers.list` carries the
        // flag + the auto-provisioned rank-0 `followers` row). One read per pass.
        let tiers = self
            .subs
            .tiers_list()
            .await
            .map_err(AuthorError::Transport)?;
        let auto: std::collections::HashSet<&str> = tiers
            .iter()
            .filter(|t| t.auto_approve)
            .map(|t| t.name.as_str())
            .collect();
        // Ruling 4 (`monetization.md` § The unifying model → *A tier may be
        // hidden*): a hidden tier is never subscribable, so `payment_entitled`
        // — which otherwise bypasses `auto_approve` below — must not bypass
        // this either. Skipped here, not just left to `approve_subscriber`'s
        // own refusal, so a lone hidden request never surfaces as the pass's
        // error (see that method's doc for why both guards exist). A skipped
        // row is left queued: on a nest that does not enforce the hidden flag this can matter for
        // (`monetization.md:284`'s `hidden-tiers` window), the request is
        // harmless residue, not a leak.
        let hidden: std::collections::HashSet<&str> = tiers
            .iter()
            .filter(|t| t.hidden)
            .map(|t| t.name.as_str())
            .collect();

        let mut committed = 0usize;
        let mut first_err: Option<AuthorError<R::Error>> = None;
        // Unsubscribes first — a leave needs no judgment, and committing the
        // removals before any approve-mint keeps a same-pass approval's KeyBlob
        // from covering a leaver the pass is about to rotate out.
        for req in &requests {
            if req.kind != UNSUBSCRIBE_KIND {
                continue;
            }
            match self.commit_unsubscribe(req).await {
                Ok(()) => committed += 1,
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        for req in &requests {
            if req.kind != SUBSCRIBE_KIND
                || hidden.contains(req.tier_name.as_str())
                || !(auto.contains(req.tier_name.as_str()) || req.payment_entitled)
            {
                continue;
            }
            match self.approve_subscriber(req).await {
                Ok(_) => committed += 1,
                Err(e) => {
                    // Skip the poison and keep draining; remember the first
                    // failure in case nothing else succeeds.
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        match first_err {
            Some(e) if committed == 0 => Err(e),
            _ => Ok(committed),
        }
    }

    /// Commit one queued subscriber-initiated leave: run the removal rotation
    /// ([`Self::remove_subscriber`] — a no-op if the leaver is already off the
    /// roster), then clear the queue row (`requests.reject` is the row-delete
    /// primitive; best-effort — a missed delete self-heals next pass, where the
    /// removal no-ops and this delete retries).
    async fn commit_unsubscribe(
        &self,
        request: &PendingRequest,
    ) -> Result<(), AuthorError<R::Error>> {
        self.remove_subscriber(&request.tier_name, request.subscriber_id)
            .await?;
        let _ = self.subs.requests_reject(request.request_id).await;
        Ok(())
    }

    /// Remove a subscriber from a tier: rotate to a fresh period key, mint a
    /// `KeyBlob` over the post-removal roster (current ∖ subscriber), upload via
    /// `subscribers.remove`, then commit the rotation into custody.
    ///
    /// Crash-safe: the fresh key is **staged + persisted before** the upload and
    /// committed only on a confirmed upload (see the module docs). Resumes an
    /// already-staged removal for this `(tier, subscriber)` rather than
    /// re-rotating. A genuine no-op (the subscriber isn't on the roster and
    /// nothing is staged) returns early without rotating.
    pub async fn remove_subscriber(
        &self,
        tier_name: &str,
        subscriber_id: ActorId,
    ) -> Result<(), AuthorError<R::Error>> {
        let mut replica = self.custody_for_write().await?;

        let removal = match custody::find_pending_removal(&replica, tier_name, &subscriber_id) {
            Some(existing) => existing.clone(),
            None => {
                // Nothing staged: skip a pointless rotation if the subscriber is
                // already absent from the roster.
                if !self
                    .roster(tier_name)
                    .await?
                    .iter()
                    .any(|(id, _)| *id == subscriber_id)
                {
                    return Ok(());
                }
                let current = custody::current_period(&replica, tier_name)
                    .ok_or_else(|| AuthorError::NoPeriodKey(tier_name.to_string()))?;
                let removal = PendingRemoval {
                    tier_name: tier_name.to_string(),
                    subscriber_id,
                    new_period: TierPeriod {
                        version: current.version + 1,
                        key: fresh_period_key().into(),
                        rotated_at: next_rotated_at(current.rotated_at),
                        // A removal mints a genuinely fresh key, so it carries
                        // this identity's stamp — the same rule
                        // `custody::rotate` applies, restated here because this
                        // path builds the period itself rather than going
                        // through that transition.
                        minted_by: Some(self.keypair.actor_id()),
                    },
                };
                // Persist the fresh (irrecoverable) key BEFORE any network upload
                // (crash-safety) — a join, so a peer's concurrent staged key is
                // never dropped.
                custody::stage_pending_removal(&mut replica, removal.clone());
                self.merge_custody(replica).await?;
                removal
            }
        };

        self.drive_removal(removal).await
    }

    /// **One author-pump tick — the whole body every app's reconcile loop runs.**
    ///
    /// Sequences the two halves in the one order that is correct, and swallows
    /// each half's failure into [`ReconcilePass`] so a shell only has to schedule
    /// it and log the result:
    ///
    /// 1. [`Self::resume_pending_removals`] — heal any subscriber-removal whose
    ///    crypto rotation + upload was interrupted (by a crash, a transient nest
    ///    rejection, **or a peer device's staging arriving by walk**).
    /// 2. [`Self::drain_auto_approvals`] — auto-approve every queued
    ///    `auto_approve` (or payment-entitled) **subscribe** request, minting the
    ///    covering `KeyBlob` per row. This is what makes an encrypted-mode
    ///    **follow** frictionless: the nest cannot mint, so a follow *enqueues*
    ///    (`Queued`) even for the `auto_approve` rank-0 `followers` tier, and the
    ///    author's own client grants it here (`monetization.md` § The unifying
    ///    model, grant path 2; § Pillar 1).
    ///
    /// **The order is load-bearing, and both halves belong in every tick.** A
    /// staged-but-uncommitted removal must be driven out *before* the drain mints
    /// over the roster, or the fresh `KeyBlob` re-covers the very subscriber
    /// being removed — they keep read access until the next tick that gets the
    /// order right. Hoisting step 1 out of the loop (running it once per
    /// connection) looks equivalent and is not: a removal staged *during* the
    /// session — the multi-device case in particular — then heals only at the
    /// next connect. Calling this instead of the two fns makes both mistakes
    /// unrepresentable.
    ///
    /// 3. **Once per connection** ([`ConnectPassLatch`]):
    ///    [`Self::republish_stale_keyed_blobs`] — heal a live blob that a stale
    ///    approve keyed under a period a removal rotated out. It runs after the
    ///    resume half so a staged removal is committed into custody first, and
    ///    is skipped on a tick whose resume failed: until that removal commits,
    ///    the live blob may wrap a key custody does not yet call `current`. It
    ///    logs its own outcome, so no shell has a third field to wire.
    ///
    /// Best-effort by construction: it returns no `Result`, because there is no
    /// caller that should stop pumping over one bad tick.
    pub async fn reconcile_once(&self) -> ReconcilePass {
        let mut pass = ReconcilePass::default();
        match self.resume_pending_removals().await {
            Ok(n) => pass.resumed = n as u32,
            Err(e) => pass.resume_error = Some(e.to_string()),
        }
        match self.drain_auto_approvals().await {
            Ok(n) => pass.approved = n as u32,
            Err(e) => pass.drain_error = Some(e.to_string()),
        }
        if pass.resume_error.is_none() && !self.connect_pass.is_done() {
            match self.republish_stale_keyed_blobs().await {
                Ok(sweep) => {
                    if sweep.republished > 0 {
                        tracing::info!(
                            "subscriptions: republished {} tier key blob(s) a stale approve \
                             had keyed under a rotated-out period",
                            sweep.republished
                        );
                    }
                    if sweep.failed == 0 {
                        self.connect_pass.mark_done();
                    }
                }
                Err(e) => tracing::warn!("subscriptions: republish_stale_keyed_blobs failed: {e}"),
            }
        }
        pass
    }

    /// **Republish every tier whose live `KeyBlob` wraps a period key this
    /// custody rotated OUT** — the key witness
    /// ([`custody::live_blob_key`]) run outside a succession
    /// (`succession-aftermath.md` § Re-key scope, the period-keys row).
    ///
    /// The window: an approve that read custody before a removal's rotation
    /// committed, but stamped a later `rotated_at`, lands the pre-removal key
    /// over the rotated blob and the nest has no reason to refuse it. The
    /// leaver's key is then live again and every subscriber is cut off from
    /// posts sealed under `current` — a delivery loss, with confidentiality
    /// intact for posts sealed before the removal. The aftermath's leg
    /// (`rotate_period_keys_after_succession`) heals the same window, but only
    /// when a succession runs; this pass is its standing twin, ticked once per
    /// connection by [`Self::reconcile_once`].
    ///
    /// Owed on exactly [`custody::LiveBlobKey::RotatedOut`]. A `Foreign` key is
    /// a peer device's unmerged newer rotation — republishing ours over it
    /// would be the regression this heals, from the other side. An unwitnessed
    /// blob is left to the aftermath leg's freshness rule. No period is minted:
    /// `current` is republished over the unchanged roster, exactly as the leg's
    /// upload-owed arm does, sharing its loop. Gated on
    /// [`capability::SUBSCRIPTION_PERIOD_ROTATE`] like the leg, since that is
    /// the door the republish goes through. Best-effort per tier.
    pub async fn republish_stale_keyed_blobs(
        &self,
    ) -> Result<StaleKeyedBlobSweep, AuthorError<R::Error>> {
        let mut sweep = StaleKeyedBlobSweep::default();
        let me = self.keypair.actor_id();
        let cfg = self.custody().await?;
        if cfg.tiers.is_empty()
            || !self
                .subs
                .nest_supports(capability::SUBSCRIPTION_PERIOD_ROTATE)
                .await
                .map_err(AuthorError::Transport)?
        {
            return Ok(sweep);
        }
        for keys in &cfg.tiers {
            let tier = keys.tier_name.as_str();
            let healed = async {
                let Some(stored) = self.stored_key_blob(me, tier).await? else {
                    return Ok(false);
                };
                if custody::live_blob_key(&cfg, tier, &stored.key_commitment)
                    != Some(custody::LiveBlobKey::RotatedOut)
                {
                    return Ok(false);
                }
                self.republish_over_roster(tier, &keys.current, Some(&stored))
                    .await?;
                Ok::<_, AuthorError<R::Error>>(true)
            };
            match healed.await {
                Ok(true) => sweep.republished += 1,
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(
                        tier = %tier,
                        error = %e,
                        "the stale-keyed-blob check failed for this tier; the next pass retries"
                    );
                    sweep.failed += 1;
                }
            }
        }
        Ok(sweep)
    }

    /// Re-drive every staged subscriber-removal whose upload was interrupted by
    /// a crash. Call on client startup (and after a walk that may have
    /// merged in a peer device's staging). Each is completed idempotently — a
    /// removal the nest already applied resolves via `not_subscribed` and just
    /// commits locally. Returns the count resumed. Stops at the first removal
    /// that exhausts its retries, leaving it staged for a later resume.
    pub async fn resume_pending_removals(&self) -> Result<usize, AuthorError<R::Error>> {
        let mut count = 0;
        loop {
            let custody_now = self.custody().await?;
            let removal = match custody_now.pending_removals.first() {
                Some(r) => r.clone(),
                None => return Ok(count),
            };
            self.drive_removal(removal).await?;
            count += 1;
        }
    }

    /// **Re-key every tier whose live period key was minted by a retired
    /// identity** — the successor-side half of the post-succession tier
    /// rotation (`succession-aftermath.md` § Re-key scope, the tier row).
    ///
    /// The ceremony *moves* the tier plane, key material included, because
    /// those keys seal the author's own back catalogue and burning them would
    /// be user-irrecoverable. That leaves the thief's copy live: a seed thief
    /// read the period keys, so without this pass every broadcast
    /// the successor seals **afterwards** opens under a key they already hold.
    /// The nest holds no period key and rotates none, so this pass is the only
    /// thing that closes it.
    ///
    /// ## Owed is DERIVED, from the KEY's own era stamp
    ///
    /// There is no progress state at rest and no debt column, matching every
    /// sibling leg (`fauna_client_recovery::aftermath`, module doc: "the corpus
    /// is its own progress record"). The stamp is [`TierPeriod::minted_by`] —
    /// the identity that minted the period key — and a tier is owed exactly
    /// when this identity did not mint the key it is currently sealing under.
    ///
    /// ⚠ **`KeyBlob.author` cannot serve, and the difference is a live
    /// vulnerability rather than a nicety**. That field
    /// records who last *published* a blob; `approve_subscriber` republishes
    /// the tier's **current** key under the caller's authorship, so after a
    /// succession one ordinary (auto-)approve stamped the stored blob with the
    /// successor's id while it still wrapped the predecessor's — the key a seed
    /// thief read — and this leg read that as "already re-keyed" and never ran
    /// again, silently. `minted_by` moves only when the key moves. For the same
    /// reason neither `KeyBlob.rotated_at` nor `KeyBlobGetReply.version` may
    /// witness the key: an approve advances both without minting anything.
    ///
    /// Deriving from a stamp rather than a clock is what makes the pass safe to
    /// re-run on **any** session: `SuccessionTime` is known only to the session
    /// that ran the ceremony, and a leg that must survive a crash cannot depend
    /// on a value the next sign-in does not have.
    ///
    /// **A period minted before the stamp existed reads as owed** (`minted_by`
    /// is `None`), which is the safe direction: one needless rotation on the
    /// first pass after the upgrade, never a missed one.
    ///
    /// ## Commit-first, deliberately unlike the removal path
    ///
    /// [`Self::remove_subscriber`] stages its fresh key in
    /// `pending_removals` and keeps sealing under the **old** one until the
    /// nest confirms, so a stalled upload never leaves the author publishing
    /// under a key no subscriber can unwrap. Here that trade inverts: the old
    /// key is the compromised one, so continuing to seal under it is the very
    /// exposure being closed. This pass therefore rotates into `current` and
    /// persists **before** the upload.
    ///
    /// That ordering is also the whole crash-safety story, which is why no
    /// sentinel is needed. The hazard the sentinel answers is a fresh key lost
    /// after the nest stored the blob — impossible here, because the key is at
    /// rest in `current` (and the outgoing one in `prior`) before the upload is
    /// attempted at all. A crash in between leaves the successor sealing under
    /// a key subscribers cannot yet unwrap; the next pass finds the key **not**
    /// owed (it minted it) but the upload still owed, and republishes the SAME
    /// period — so the content sealed meanwhile becomes readable the moment it
    /// lands. The delivery gap heals retroactively, whereas a confidentiality
    /// leak never would, and one succession costs one key per tier.
    ///
    /// That second question is the only one the nest answers here, by whether
    /// its stored blob is older than the local `current` — a FRESHNESS test,
    /// legitimate precisely because it runs after `minted_by` has already
    /// settled whose key is live.
    ///
    /// Best-effort per tier: a tier that fails is counted and the pass moves
    /// on, because one unreachable tier must not strand the rest.
    pub async fn rotate_period_keys_after_succession(
        &self,
    ) -> Result<PeriodRotationOutcome, AuthorError<R::Error>> {
        let me = self.keypair.actor_id();

        let tiers: Vec<String> = self
            .custody()
            .await?
            .tiers
            .iter()
            .map(|t| t.tier_name.clone())
            .collect();
        if tiers.is_empty() {
            return Ok(PeriodRotationOutcome::NothingHeld);
        }

        // The version brake, checked once. Against a nest that does not serve
        // the door the pass is a clean no-op: rotating without being able to
        // publish would leave the successor sealing under a key no subscriber
        // can reach, with nothing to distinguish a nest without the door from
        // an outage.
        if !self
            .subs
            .nest_supports(capability::SUBSCRIPTION_PERIOD_ROTATE)
            .await
            .map_err(AuthorError::Transport)?
        {
            return Ok(PeriodRotationOutcome::NestCannotRotate);
        }

        let mut outcome = PeriodRotationOutcome::Swept {
            rotated: 0,
            already_current: 0,
            failed: 0,
        };
        for tier in tiers {
            match self.rotate_one_tier_after_succession(&tier, me).await {
                Ok(true) => outcome.count_rotated(),
                Ok(false) => outcome.count_already_current(),
                Err(e) => {
                    tracing::warn!(
                        tier = %tier,
                        error = %e,
                        "the post-succession period-key rotation failed for this tier; the next \
                         pass re-derives it from the period key's minter and retries"
                    );
                    outcome.count_failed();
                }
            }
        }
        Ok(outcome)
    }

    /// One tier's leg of [`Self::rotate_period_keys_after_succession`].
    /// `Ok(true)` when it rotated + republished, `Ok(false)` when this identity
    /// already minted the live period and its upload had landed.
    async fn rotate_one_tier_after_succession(
        &self,
        tier: &str,
        me: ActorId,
    ) -> Result<bool, AuthorError<R::Error>> {
        // ── Is the KEY owed? Asked of custody, never of the nest ────────────
        //
        // `TierPeriod::minted_by` is the only stamp that moves when the KEY
        // moves. `KeyBlob.author` moves when the BLOB moves, and those are not
        // the same event: `approve_subscriber` republishes the tier's current
        // key under the caller's authorship, so after a succession one
        // (auto-)approve stamped the stored blob with the successor's id while
        // it still wrapped the predecessor's key — and this leg read that as
        // "already re-keyed" and never ran again. The
        // window needed no deferral: the author pump drains auto-approvals on
        // connect and every 30 s, unsequenced against this leg, so one arriving
        // follower on the reserved `followers` tier could close it inside the
        // successor's very first session.
        let mut cfg = self.custody().await?;
        let current = custody::current_period(&cfg, tier)
            .ok_or_else(|| AuthorError::NoPeriodKey(tier.to_string()))?;
        let key_owed = !current.was_minted_by(&me);

        // ── Is the UPLOAD owed? Asked of the stored blob's KEY WITNESS ──────
        //
        // Only reached when the key is not owed, i.e. this identity minted the
        // live period — so the one remaining question is whether the blob the
        // nest serves wraps THAT key. It may not: a crash between the rotation
        // and its upload leaves the previous blob live (the commit-first
        // ordering deliberately allows it), and an approve that read custody
        // before a rotation but stamped a LATER `rotated_at` lands the
        // pre-rotation key over the rotated blob with no refusal at all. Neither `author` (an approve
        // re-stamps it) nor `rotated_at` (an approve advances it) can tell
        // those apart, so the blob carries a commitment to the key it wraps
        // (`KeyBlob::key_commitment`) and `custody::live_blob_key` reads it
        // against this custody's history — settled, rotated-out (owed), or
        // foreign (a peer's newer key: not ours to regress). A missing blob
        // is not owed here: a tier whose author minted a key and never had a
        // subscriber legitimately has none.
        let stored = self.stored_key_blob(me, tier).await?;
        let upload_owed = stored.as_ref().is_some_and(|b| {
            match custody::live_blob_key(&cfg, tier, &b.key_commitment) {
                Some(custody::LiveBlobKey::Current) => false,
                Some(custody::LiveBlobKey::RotatedOut) => true,
                Some(custody::LiveBlobKey::Foreign) => false,
                // Custody has no entry for the tier; unreachable past the
                // `current_period` read above, and owed if it ever were.
                None => true,
            }
        });
        if !key_owed && !upload_owed {
            return Ok(false);
        }

        let period = if key_owed {
            // Persisted BEFORE the upload: from here the fresh key is at rest
            // and the outgoing one is retained in `prior`, so no crash can lose
            // either. A join, so a peer's concurrent write is never dropped.
            let next = custody::rotate(
                &mut cfg,
                me,
                tier,
                *fresh_period_key(),
                next_rotated_at(current.rotated_at),
            )
            .map_err(AuthorError::Custody)?;
            self.merge_custody(cfg).await?;
            next
        } else {
            // The key is already this identity's; only its publication is owed.
            // Republish the SAME period rather than minting a second one.
            current
        };

        self.republish_over_roster(tier, &period, stored.as_ref())
            .await?;
        Ok(true)
    }

    /// The tier's live `KeyBlob` as the nest serves it to its author, decoded —
    /// `None` only on the nest's own `key_blob_not_found`. A blob that will not
    /// decode is a [`AuthorError::BlobDecode`], never "no blob": both callers
    /// ask it whether a republish is owed, and guessing "not owed" on a parse
    /// or transport fault would retire the exposure silently.
    async fn stored_key_blob(
        &self,
        me: ActorId,
        tier: &str,
    ) -> Result<Option<KeyBlob>, AuthorError<R::Error>> {
        match self.subs.key_blob_get(me, tier).await {
            Ok(reply) => {
                let wire: EmbedAsBytes = canonical_decode(reply.blob_data.as_ref())
                    .map_err(|e| AuthorError::BlobDecode(format!("key blob wire: {e}")))?;
                Ok(Some(decode_signed_bytes(&wire.bytes).map_err(|e| {
                    AuthorError::BlobDecode(format!("key blob: {e}"))
                })?))
            }
            Err(e) if retryable_code(&e).as_deref() == Some(KEY_BLOB_NOT_FOUND) => Ok(None),
            Err(e) => Err(AuthorError::Transport(e)),
        }
    }

    /// Publish `period` as `tier`'s live `KeyBlob` over the UNCHANGED roster
    /// through `key_blob.rotate`, then re-stamp custody's `current` with the
    /// winning `rotated_at`. The shared tail of the succession leg and of
    /// [`Self::republish_stale_keyed_blobs`]; `stored` is the live blob the
    /// caller already read, if any.
    async fn republish_over_roster(
        &self,
        tier: &str,
        period: &TierPeriod,
        stored: Option<&KeyBlob>,
    ) -> Result<(), AuthorError<R::Error>> {
        // `roster_mismatch` means the roster moved under us (another device
        // approved or removed someone): re-read and re-mint. `stale_rotation`
        // means another device published first: advance past it. The stored
        // blob's stamp is already in hand, and a stale-keyed blob's is LATER
        // than the period's (that is how it got past the nest), so start above
        // it rather than spend a refused round trip learning what was just read.
        let mut rotated_at = stored.map_or(period.rotated_at, |b| {
            period.rotated_at.max(next_rotated_at(b.rotated_at.0))
        });
        let mut last_code = String::new();
        let mut applied = false;
        for _ in 0..MAX_MINT_ATTEMPTS {
            let subscribers = self.roster(tier).await?;
            let upload = self.mint_upload(tier, rotated_at, &subscribers, &period.key)?;
            match self.subs.key_blob_rotate(tier, upload).await {
                Ok(_) => {
                    applied = true;
                    break;
                }
                Err(e) => match retryable_code(&e).as_deref() {
                    Some(ROSTER_MISMATCH) => {
                        rotated_at = next_rotated_at(rotated_at);
                        last_code = ROSTER_MISMATCH.into();
                    }
                    Some(STALE_ROTATION) => {
                        rotated_at = next_rotated_at(rotated_at) + STALE_BUMP_MICROS;
                        last_code = STALE_ROTATION.into();
                    }
                    _ => return Err(AuthorError::Transport(e)),
                },
            }
        }
        if !applied {
            return Err(AuthorError::RetriesExhausted { code: last_code });
        }

        // The local `current` must carry what the winning blob carried — the
        // invariant `drive_removal` keeps. A re-stamp, not a re-commit: the
        // fold reads the two stamps of one mint as one period (see
        // `custody::restamp_current`).
        if rotated_at != period.rotated_at {
            let mut cfg = self.custody().await?;
            if custody::restamp_current(&mut cfg, tier, period.version, rotated_at) {
                self.merge_custody(cfg).await?;
            }
        }
        Ok(())
    }

    /// The mint+upload+commit core shared by [`Self::remove_subscriber`] and
    /// [`Self::resume_pending_removals`]: drive the staged `removal` to a
    /// confirmed nest upload, then commit the rotation into `current` and
    /// settle the sentinel. Both are joins into the custody store (the
    /// committed period a period row, the staging's `settled` marker set), so
    /// the commit converges with any concurrent peer-device write and a stale
    /// replica's unsettled copy of the sentinel never brings it back.
    async fn drive_removal(&self, removal: PendingRemoval) -> Result<(), AuthorError<R::Error>> {
        let mut rotated_at = removal.new_period.rotated_at;
        let mut last_code = String::new();
        let mut applied = false;
        for _ in 0..MAX_MINT_ATTEMPTS {
            let subscribers: Vec<(ActorId, Option<Vec<u8>>)> = self
                .roster(&removal.tier_name)
                .await?
                .into_iter()
                .filter(|(id, _)| *id != removal.subscriber_id)
                .collect();
            let upload = self.mint_upload(
                &removal.tier_name,
                rotated_at,
                &subscribers,
                &removal.new_period.key,
            )?;
            match self
                .subs
                .subscribers_remove(&removal.tier_name, removal.subscriber_id, Some(upload))
                .await
            {
                Ok(_) => {
                    applied = true;
                    break;
                }
                Err(e) => match retryable_code(&e).as_deref() {
                    // A prior (crash-interrupted) attempt of THIS removal already
                    // applied it — the committed state is the rotation.
                    Some(NOT_SUBSCRIBED) => {
                        applied = true;
                        break;
                    }
                    Some(ROSTER_MISMATCH) => {
                        rotated_at = next_rotated_at(rotated_at);
                        last_code = ROSTER_MISMATCH.into();
                    }
                    Some(STALE_ROTATION) => {
                        rotated_at = next_rotated_at(rotated_at) + STALE_BUMP_MICROS;
                        last_code = STALE_ROTATION.into();
                    }
                    _ => return Err(AuthorError::Transport(e)),
                },
            }
        }
        if !applied {
            return Err(AuthorError::RetriesExhausted { code: last_code });
        }

        // Commit: the nest holds the new blob; move the staged period into
        // `current` (idempotent) with the `rotated_at` the winning blob
        // carried, then settle the sentinel. The commit reads the custody
        // fresh, so it applies atop the latest rows (the staged sentinel that
        // was persisted before the upload included) and joins with any
        // concurrent peer-device write. Settling writes the staged period's
        // own row too; beside a re-stamped commit the fold reads the two as
        // one period at the winning stamp.
        // Struct-update: everything else — the key, the version and the MINTER
        // — is the staged period's, unchanged. A commit is not a mint.
        let committed = TierPeriod {
            rotated_at,
            ..removal.new_period.clone()
        };
        let mut cfg = self.custody().await?;
        custody::commit_period(&mut cfg, &removal.tier_name, &committed)
            .map_err(AuthorError::Custody)?;
        self.merge_custody(cfg).await?;
        self.period_keys
            .settle_removal(removal)
            .await
            .map_err(AuthorError::PeriodKeys)?;
        Ok(())
    }

    /// The current confirmed subscriber roster for `tier`, as `(actor id,
    /// published ML-KEM ek)` pairs — the ek (S4c-2-surfaced on
    /// `SubscriberEntry`) feeds per-member hybrid suite selection in
    /// [`Self::mint_upload`].
    async fn roster(
        &self,
        tier: &str,
    ) -> Result<Vec<(ActorId, Option<Vec<u8>>)>, AuthorError<R::Error>> {
        Ok(self
            .subs
            .subscribers_list(tier)
            .await
            .map_err(AuthorError::Transport)?
            .into_iter()
            .map(|e| (e.subscriber_id, e.mlkem_encaps_key.map(|b| b.into_vec())))
            .collect())
    }
}

/// A fresh 32-byte broadcast period key. Kept out of the pure `custody`
/// transitions so those stay deterministic + RNG-free.
///
/// The CSPRNG call and the non-`Copy` *Carrier shape* rule belong to
/// [`fauna_core::secret::fresh_secret_32`], shared with the two sibling minters
/// (`fresh_msek`, `fresh_content_key`) rather than restated here.
pub fn fresh_period_key() -> Zeroizing<[u8; 32]> {
    fauna_core::secret::fresh_secret_32()
}

/// Compile-time pin that [`fresh_period_key`] keeps handing its output out
/// non-`Copy` — see `key-material-hierarchy.md` § Carrier shape → *Pinned at
/// compile time*.
const _FRESH_PERIOD_KEY_IS_NOT_COPY: fn() -> Zeroizing<[u8; 32]> = fresh_period_key;

/// The name prefix every per-post pay-to-unlock tier carries.
///
/// A designated tier is hidden from every generic tier surface
/// (`monetization.md:128`), so this name is not a browse label — but it *is*
/// what a gated post's `gated-post-badge` renders today, and it is the custody
/// key, so it must be stable, unique, and free of anything derived from the
/// post's sealed text.
pub const UNLOCK_TIER_PREFIX: &str = "post-unlock-";

/// Mint a fresh, unique name for a per-post pay-to-unlock tier.
///
/// The name must exist *before* the post it sells: the gated body names its
/// tier, and the post id is the hash of that body (`monetization.md` § Per-post
/// pay-to-unlock — the forced ordering). So it cannot be derived from the post,
/// and it deliberately is **not** derived from the post's text either — the
/// name is world-readable on the gated post's badge, and a hash of the sealed
/// plaintext would be a commitment that a short body could be brute-forced
/// against. Random it is: 16 hex chars of `thread_rng` (the `fresh_period_key`
/// source), 28 chars total — well inside the nest's 64-char bound
/// (`bins/fauna-nest/src/subscription_handlers.rs:838`).
///
/// There is deliberately **no** collision-retry loop: at 2^-64 per mint it
/// would be untestable dead code. A collision surfaces as the nest's ordinary
/// `tier_already_exists` rejection, which the compose error surfaces and the
/// user's retry resolves by minting a fresh name.
pub fn mint_unlock_tier_name() -> String {
    let mut raw = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut raw);
    format!("{UNLOCK_TIER_PREFIX}{}", hex::encode(raw))
}

/// The wire code if `e` is a server rejection (else `None` — a transport fault),
/// so the retry loop can match `roster_mismatch` / `stale_rotation` /
/// `not_subscribed` transport-agnostically.
fn retryable_code<E: RpcErrorClass>(e: &E) -> Option<String> {
    e.as_rpc_error().map(|r| r.code.clone())
}

/// A `rotated_at` (micros) strictly above `floor` and at least the wall clock —
/// keeps each mint attempt monotonic against both the prior period and the
/// stored blob.
fn next_rotated_at(floor: u64) -> u64 {
    Timestamp::now().0.max(floor.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::period_keys::{MemoryPeriodKeyStore, PeriodKeyRows, PeriodKeyStore};
    use fauna_client_testkit::block_on;
    use fauna_core::data::SubscriptionsConfig;
    use fauna_core::encoding::decode_signed_bytes;
    use fauna_core::identity::ActorKeypair;
    use fauna_core::subscription::crypto::{
        decrypt_key_blob_entry_for, subscriber_mlkem_encaps_key,
    };
    use fauna_core::subscription::types::{KemSuiteId, KeyBlob};
    use fauna_protocol::ByteBuf;
    use fauna_protocol::discovery::{ModerationInfo, NestInfoReply};
    use fauna_protocol::error::RpcError;
    use fauna_protocol::subscriptions::{
        ApproveRequestReply, ApproveRequestRequest, KeyBlobGetReply, KeyBlobGetRequest,
        RejectRequestReply, RejectRequestRequest, RemoveSubscriberReply, RemoveSubscriberRequest,
        RequestsListReply, RotateKeyBlobReply, RotateKeyBlobRequest, SubscriberEntry,
        SubscribersListReply, TierCreateReply, TierCreateRequest, TierItem, TiersListReply,
    };
    use std::collections::{BTreeSet, HashMap};
    use std::sync::{Arc, Mutex};

    /// A transport error that classifies as a server rejection carrying a wire
    /// code — what the retry loop matches `roster_mismatch` / `stale_rotation` /
    /// `not_subscribed` against. (`Infallible`, the wire-contract mock's error,
    /// can't carry one.)
    #[derive(Debug)]
    enum FakeError {
        Rpc(RpcError),
        /// A fault that never reached the nest (a dropped connection, a
        /// deadline) — carries no wire code, which is exactly what
        /// `live_key_blob_ref_if_any` must NOT read as "no blob".
        Transport,
    }
    impl core::fmt::Display for FakeError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                FakeError::Rpc(e) => write!(f, "{}", e.code),
                FakeError::Transport => write!(f, "transport"),
            }
        }
    }
    impl RpcErrorClass for FakeError {
        fn is_rejection(&self) -> bool {
            matches!(self, FakeError::Rpc(_))
        }
        fn as_rpc_error(&self) -> Option<&RpcError> {
            match self {
                FakeError::Rpc(e) => Some(e),
                FakeError::Transport => None,
            }
        }
    }

    /// One in-process fake nest answering the `fauna.subscriptions.*` kinds the
    /// orchestration drives. The
    /// `*_script` queues let a test inject per-call nest rejections (a concurrent
    /// device moving the roster / rotation under us) keyed by call index;
    /// exhausted/absent ⇒ success. Successful `subscribers.remove` mutates the
    /// roster, mirroring the real nest, so resume's `not_subscribed` path is real.
    #[derive(Default)]
    struct FakeNest {
        /// The account's period-key custody, one store for every author over
        /// this fake — the `fauna.state.subscriptions` rows two devices of one
        /// account converge on, with the production door's join semantics.
        period_keys: MemoryPeriodKeyStore,
        roster: Mutex<Vec<ActorId>>,
        /// Per-subscriber published ek surfaced on `subscribers.list` (S4b).
        roster_eks: Mutex<HashMap<[u8; 32], Vec<u8>>>,
        /// Whether `fauna.nest.info` advertises `subscription-period-rotate`
        /// (the succession rotation's version brake). Default **off** — a nest
        /// without the door, the state the leg must no-op against.
        period_rotate: Mutex<bool>,
        approve_script: Mutex<Vec<Option<&'static str>>>,
        remove_script: Mutex<Vec<Option<&'static str>>>,
        rotate_script: Mutex<Vec<Option<&'static str>>>,
        approve_uploads: Mutex<Vec<EncryptedKeyBlobUpload>>,
        remove_uploads: Mutex<Vec<EncryptedKeyBlobUpload>>,
        rotate_uploads: Mutex<Vec<EncryptedKeyBlobUpload>>,
        approve_calls: Mutex<usize>,
        remove_calls: Mutex<usize>,
        rotate_calls: Mutex<usize>,
        /// Every `requests.reject` request id — the drain's unsubscribe-commit
        /// cleanup leg, so a test can assert the queue row was cleared.
        reject_calls: Mutex<Vec<i64>>,
        tiers_create_calls: Mutex<usize>,
        /// Every `tiers.create` request body, so a test can assert what the
        /// orchestration actually sent (the `unlocks_post` designation and the
        /// staged birth `KeyBlob`), not merely that it called.
        tiers_create_reqs: Mutex<Vec<TierCreateRequest>>,
        /// Author's pending subscribe/unsubscribe queue (`requests.list`) — what
        /// `drain_auto_approvals` iterates.
        pending_requests: Mutex<Vec<PendingRequest>>,
        /// Author's own tier definitions (`tiers.list`) — the `auto_approve`
        /// source the drain resolves against (`requests.list` carries no flag).
        tiers: Mutex<Vec<TierItem>>,
        /// The stored `current_key_blobs` rows, keyed by tier name (this fake
        /// serves one author). Written by the `tiers.create` arm from the birth
        /// blob, like the nest's `upsert_current_key_blob`.
        key_blobs: Mutex<HashMap<String, StoredKeyBlob>>,
        /// How many upcoming `key_blob.get` calls fail with a code-less
        /// transport fault before the fake answers normally again.
        key_blob_get_faults: Mutex<usize>,
        /// When set, a `tiers.create` stores THIS blob hash instead of the
        /// upload's — the nest that kept a blob a sibling device landed between
        /// this client's read and its create, answering `created: false` with
        /// no other signal.
        foreign_blob_on_create: Mutex<Option<[u8; 32]>>,
        /// When set, `requests.approve` behaves like the nest's upload doors:
        /// it refuses an upload whose `rotated_at` does not strictly advance
        /// the stored blob's (`stale_rotation`) and otherwise STORES it as the
        /// tier's live blob. Off by default, so the approve tests that never
        /// stock a blob keep their scripted-only fake.
        approve_keeps_blob: Mutex<bool>,
        /// The custody rows to swap in at the NEXT `requests.approve` call,
        /// before it is answered — a rotation's rows reaching this device
        /// between the approve's custody read and its upload, which is the
        /// race the approve's `stale_rotation` arm has to survive.
        custody_on_approve: Mutex<Option<PeriodKeyRows>>,
        /// When set, `subscribers.remove` STORES its upload as the tier's live
        /// blob (refusing a non-advancing `rotated_at` as `stale_rotation`),
        /// like the nest's removal door. Off by default, like
        /// `approve_keeps_blob`.
        remove_keeps_blob: Mutex<bool>,
        /// Every `key_blob.get` served — so a test can see whether a pass ran
        /// at all, not only whether it published.
        key_blob_get_calls: Mutex<usize>,
    }

    /// One `current_key_blobs` row as `key_blob.get` answers it:
    /// `(version, blob_hash, blob_data)`.
    type StoredKeyBlob = (u64, [u8; 32], Vec<u8>);

    impl FakeNest {
        fn set_roster(&self, subs: &[ActorId]) {
            *self.roster.lock().unwrap() = subs.to_vec();
        }
        fn set_period_rotate(&self, v: bool) {
            *self.period_rotate.lock().unwrap() = v;
        }
        /// Stock a **real, signed** `KeyBlob` for `tier`, authored by `signer`,
        /// wrapping `key`. The rotation no longer derives what it owes from the
        /// author (it reads `TierPeriod::minted_by`, and of the stored blob only
        /// its `rotated_at`), so the author staged here is never what decides.
        fn set_authored_key_blob(
            &self,
            tier: &str,
            signer: &ActorKeypair,
            subscribers: &[ActorId],
            rotated_at: u64,
            key: [u8; 32],
        ) {
            let signed = build_manage_subscribers_self_delegation(signer, Timestamp::now())
                .expect("self delegation");
            let eks: Vec<Option<[u8; MLKEM768_ENCAPS_KEY_LEN]>> =
                subscribers.iter().map(|_| None).collect();
            let minted = mint_key_blob(
                signer,
                &signed.auth,
                &signed.bytes,
                &signed.envelope,
                tier.to_string(),
                Timestamp(rotated_at),
                subscribers,
                &eks,
                &key,
            )
            .expect("mint stocked blob");
            let (bytes, envelope) = fauna_core::encoding::sign_envelope(signer, &minted.blob)
                .expect("re-sign stocked blob");
            let wire = EmbedAsBytes::from_signed(bytes, envelope);
            let data = fauna_protocol::encode_canonical(&wire)
                .expect("encode stocked blob")
                .to_vec();
            self.key_blobs
                .lock()
                .unwrap()
                .insert(tier.to_string(), (1, [0u8; 32], data));
        }
        /// The tier's live blob as the fake stores it, decoded.
        fn live_blob(&self, tier: &str) -> Option<KeyBlob> {
            let blobs = self.key_blobs.lock().unwrap();
            let (_, _, data) = blobs.get(tier)?;
            let wire: EmbedAsBytes = canonical_decode(data).expect("decode stored wire");
            Some(decode_signed_bytes(&wire.bytes).expect("decode stored blob"))
        }
        /// Every `KeyBlob` the client published through `key_blob.rotate`.
        fn rotated_blobs(&self) -> Vec<KeyBlob> {
            self.rotate_uploads
                .lock()
                .unwrap()
                .iter()
                .map(|u| decode_signed_bytes(&u.key_blob.bytes).expect("decode rotated blob"))
                .collect()
        }
        fn set_subscriber_ek(&self, sub: ActorId, ek: Vec<u8>) {
            self.roster_eks.lock().unwrap().insert(sub.0, ek);
        }
        fn set_requests(&self, requests: &[PendingRequest]) {
            *self.pending_requests.lock().unwrap() = requests.to_vec();
        }
        fn set_tiers(&self, tiers: &[TierItem]) {
            *self.tiers.lock().unwrap() = tiers.to_vec();
        }
        /// Stock a live `KeyBlob` for `tier` without going through a create —
        /// the "the nest already holds a blob" state a fresh client walks into
        /// (a sibling device minted it, or this device lost its custody copy).
        fn set_key_blob(&self, tier: &str, version: u64, hash: [u8; 32]) {
            self.key_blobs
                .lock()
                .unwrap()
                .insert(tier.to_string(), (version, hash, b"stocked-blob".to_vec()));
        }
        /// The next `n` `key_blob.get` calls fail at the transport.
        fn fail_key_blob_get(&self, n: usize) {
            *self.key_blob_get_faults.lock().unwrap() = n;
        }
        /// Every `tiers.create` from now on stores `hash` as the tier's live
        /// blob, whatever the upload carried.
        fn keep_foreign_blob_on_create(&self, hash: [u8; 32]) {
            *self.foreign_blob_on_create.lock().unwrap() = Some(hash);
        }
        /// The upload doors' shared store: refuse an upload whose `rotated_at`
        /// does not strictly advance the live blob's, else make it live.
        fn store_upload_as_live(
            &self,
            tier: &str,
            up: &EncryptedKeyBlobUpload,
        ) -> Result<(), FakeError> {
            let incoming: KeyBlob = decode_signed_bytes(&up.key_blob.bytes).expect("decode upload");
            if let Some(prior) = self.live_blob(tier)
                && incoming.rotated_at.0 <= prior.rotated_at.0
            {
                return Err(FakeError::Rpc(RpcError::new(STALE_ROTATION, "error.test")));
            }
            let wire = EmbedAsBytes {
                envelope: up.key_blob.envelope.clone(),
                bytes: up.key_blob.bytes.clone(),
                signer_auth: None,
            };
            let data = fauna_protocol::encode_canonical(&wire)
                .expect("encode stored blob")
                .to_vec();
            let mut blobs = self.key_blobs.lock().unwrap();
            let version = blobs.get(tier).map(|(v, _, _)| *v).unwrap_or(0) + 1;
            blobs.insert(tier.to_string(), (version, [0u8; 32], data));
            Ok(())
        }
        fn next_idx(slot: &Mutex<usize>) -> usize {
            let mut c = slot.lock().unwrap();
            let n = *c;
            *c += 1;
            n
        }
        fn scripted(script: &Mutex<Vec<Option<&'static str>>>, idx: usize) -> Option<&'static str> {
            script.lock().unwrap().get(idx).copied().flatten()
        }
    }

    impl RpcRequester for FakeNest {
        type Error = FakeError;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            let out: Vec<u8> = match kind {
                "fauna.subscriptions.tiers.create" => {
                    *self.tiers_create_calls.lock().unwrap() += 1;
                    let req: TierCreateRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode tiers.create");
                    // Mirror the nest's create: the row joins `tiers.list` and
                    // the birth blob becomes the tier's live `KeyBlob` (only
                    // when it has none, so a repeat never rolls one back).
                    let mut tiers = self.tiers.lock().unwrap();
                    let created = !tiers.iter().any(|t| t.name == req.name);
                    if created {
                        tiers.push(TierItem {
                            name: req.name.clone(),
                            rank: req.rank,
                            description: req.description.clone(),
                            price_hint: req.price_hint.clone(),
                            payment_url: req.payment_url.clone(),
                            auto_approve: req.auto_approve,
                            created_at: Timestamp(0),
                            unlocks_post: req.unlocks_post.clone(),
                            asking_price: req.asking_price.clone(),
                            hidden: req.hidden,
                            extra: Default::default(),
                        });
                    }
                    drop(tiers);
                    {
                        let upload = &req.encrypted_upload;
                        let foreign = *self.foreign_blob_on_create.lock().unwrap();
                        let mut blobs = self.key_blobs.lock().unwrap();
                        blobs
                            .entry(req.name.clone())
                            .or_insert_with(|| match foreign {
                                Some(hash) => (1, hash, b"foreign-blob".to_vec()),
                                // Stored in the wire shape `key_blob.get`
                                // serves, like the upload doors below.
                                None => (
                                    1,
                                    fauna_core::encoding::content_hash(&upload.key_blob.bytes)
                                        .digest(),
                                    fauna_protocol::encode_canonical(&EmbedAsBytes {
                                        envelope: upload.key_blob.envelope.clone(),
                                        bytes: upload.key_blob.bytes.clone(),
                                        signer_auth: None,
                                    })
                                    .expect("encode stored blob")
                                    .to_vec(),
                                ),
                            });
                    }
                    self.tiers_create_reqs.lock().unwrap().push(req);
                    fauna_protocol::encode_canonical(&TierCreateReply {
                        extra: Default::default(),
                        created,
                    })
                    .unwrap()
                    .to_vec()
                }
                "fauna.subscriptions.key_blob.get" => {
                    *self.key_blob_get_calls.lock().unwrap() += 1;
                    {
                        let mut faults = self.key_blob_get_faults.lock().unwrap();
                        if *faults > 0 {
                            *faults -= 1;
                            return Err(FakeError::Transport);
                        }
                    }
                    let req: KeyBlobGetRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode key_blob.get");
                    let blobs = self.key_blobs.lock().unwrap();
                    let Some((version, hash, data)) = blobs.get(&req.tier_name) else {
                        return Err(FakeError::Rpc(RpcError::new(
                            "fauna.subscriptions.key_blob_not_found",
                            "error.test",
                        )));
                    };
                    fauna_protocol::encode_canonical(&KeyBlobGetReply {
                        version: *version,
                        blob_hash: ByteBuf::from(hash.to_vec()),
                        blob_data: ByteBuf::from(data.clone()),
                        extra: Default::default(),
                    })
                    .unwrap()
                    .to_vec()
                }
                "fauna.nest.info" => {
                    let reply = NestInfoReply {
                        domain: "test".into(),
                        nest_id: String::new(),
                        version: String::new(),
                        software: "fauna".into(),
                        protocols: vec!["fauna".into()],
                        capabilities: {
                            let mut caps = Vec::new();
                            if *self.period_rotate.lock().unwrap() {
                                caps.push(capability::SUBSCRIPTION_PERIOD_ROTATE.to_string());
                            }
                            caps
                        },
                        iroh_relay_url: None,
                        subhandles: false,
                        registration: None,
                        moderation: ModerationInfo {
                            extra: Default::default(),
                        },
                        ..Default::default()
                    };
                    fauna_protocol::encode_canonical(&reply).unwrap().to_vec()
                }
                "fauna.subscriptions.subscribers.list" => {
                    let eks = self.roster_eks.lock().unwrap();
                    let subs: Vec<SubscriberEntry> = self
                        .roster
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|s| SubscriberEntry {
                            extra: Default::default(),
                            subscriber_id: *s,
                            joined_at: Timestamp(0),
                            mlkem_encaps_key: eks.get(&s.0).cloned().map(ByteBuf::from),
                        })
                        .collect();
                    fauna_protocol::encode_canonical(&SubscribersListReply {
                        extra: Default::default(),
                        subscribers: subs,
                    })
                    .unwrap()
                    .to_vec()
                }
                "fauna.subscriptions.requests.list" => {
                    let requests = self.pending_requests.lock().unwrap().clone();
                    fauna_protocol::encode_canonical(&RequestsListReply {
                        extra: Default::default(),
                        requests,
                    })
                    .unwrap()
                    .to_vec()
                }
                "fauna.subscriptions.tiers.list" => {
                    let tiers = self.tiers.lock().unwrap().clone();
                    fauna_protocol::encode_canonical(&TiersListReply {
                        extra: Default::default(),
                        tiers,
                    })
                    .unwrap()
                    .to_vec()
                }
                "fauna.subscriptions.requests.approve" => {
                    let req: ApproveRequestRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode approve");
                    if let Some(rows) = self.custody_on_approve.lock().unwrap().take() {
                        self.period_keys.restore(rows);
                    }
                    if let Some(up) = req.encrypted_upload.clone() {
                        self.approve_uploads.lock().unwrap().push(up);
                    }
                    let n = Self::next_idx(&self.approve_calls);
                    if let Some(code) = Self::scripted(&self.approve_script, n) {
                        return Err(FakeError::Rpc(RpcError::new(code, "error.test")));
                    }
                    if *self.approve_keeps_blob.lock().unwrap() {
                        let up = req.encrypted_upload.expect("client-minted approve");
                        let incoming: KeyBlob =
                            decode_signed_bytes(&up.key_blob.bytes).expect("decode approve blob");
                        if let Some(prior) = self.live_blob(&incoming.tier)
                            && incoming.rotated_at.0 <= prior.rotated_at.0
                        {
                            return Err(FakeError::Rpc(RpcError::new(
                                STALE_ROTATION,
                                "error.test",
                            )));
                        }
                        let wire = EmbedAsBytes {
                            envelope: up.key_blob.envelope.clone(),
                            bytes: up.key_blob.bytes.clone(),
                            signer_auth: None,
                        };
                        let data = fauna_protocol::encode_canonical(&wire)
                            .expect("encode stored blob")
                            .to_vec();
                        let mut blobs = self.key_blobs.lock().unwrap();
                        let version =
                            blobs.get(&incoming.tier).map(|(v, _, _)| *v).unwrap_or(0) + 1;
                        blobs.insert(incoming.tier.clone(), (version, [0u8; 32], data));
                    }
                    fauna_protocol::encode_canonical(&ApproveRequestReply {
                        extra: Default::default(),
                        subscriber: ActorId([0; 32]),
                        tier: "t".into(),
                        key_version: 1,
                    })
                    .unwrap()
                    .to_vec()
                }
                "fauna.subscriptions.key_blob.rotate" => {
                    let req: RotateKeyBlobRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode rotate");
                    self.rotate_uploads
                        .lock()
                        .unwrap()
                        .push(req.encrypted_upload.clone());
                    let n = Self::next_idx(&self.rotate_calls);
                    if let Some(code) = Self::scripted(&self.rotate_script, n) {
                        return Err(FakeError::Rpc(RpcError::new(code, "error.test")));
                    }
                    // Mirror the nest's door: `rotated_at` must strictly advance
                    // the stored blob's, or the upload is `stale_rotation` — so
                    // a republish over a stale-keyed blob that carries a LATER
                    // stamp has to climb past it here as it would there.
                    let incoming: KeyBlob =
                        decode_signed_bytes(&req.encrypted_upload.key_blob.bytes)
                            .expect("decode rotate blob");
                    if let Some(prior) = self.live_blob(&req.tier_name)
                        && incoming.rotated_at.0 <= prior.rotated_at.0
                    {
                        return Err(FakeError::Rpc(RpcError::new(STALE_ROTATION, "error.test")));
                    }
                    // Mirror the nest: the accepted upload BECOMES the tier's
                    // live blob, so the era stamp a later pass reads back is the
                    // one this client just published — which is what makes the
                    // idempotence assertion real rather than staged.
                    let wire = EmbedAsBytes {
                        envelope: req.encrypted_upload.key_blob.envelope.clone(),
                        bytes: req.encrypted_upload.key_blob.bytes.clone(),
                        signer_auth: None,
                    };
                    let data = fauna_protocol::encode_canonical(&wire)
                        .expect("encode stored blob")
                        .to_vec();
                    let mut blobs = self.key_blobs.lock().unwrap();
                    let version = blobs.get(&req.tier_name).map(|(v, _, _)| *v).unwrap_or(0) + 1;
                    blobs.insert(req.tier_name.clone(), (version, [0u8; 32], data));
                    drop(blobs);
                    fauna_protocol::encode_canonical(&RotateKeyBlobReply {
                        extra: Default::default(),
                        tier: req.tier_name,
                        key_version: version,
                    })
                    .unwrap()
                    .to_vec()
                }
                "fauna.subscriptions.subscribers.remove" => {
                    let req: RemoveSubscriberRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode remove");
                    if let Some(up) = req.encrypted_upload.clone() {
                        self.remove_uploads.lock().unwrap().push(up);
                    }
                    let n = Self::next_idx(&self.remove_calls);
                    if let Some(code) = Self::scripted(&self.remove_script, n) {
                        return Err(FakeError::Rpc(RpcError::new(code, "error.test")));
                    }
                    if *self.remove_keeps_blob.lock().unwrap() {
                        let up = req.encrypted_upload.clone().expect("client-minted removal");
                        self.store_upload_as_live(&req.tier_name, &up)?;
                    }
                    // Mirror the nest: a confirmed removal drops the subscriber.
                    self.roster
                        .lock()
                        .unwrap()
                        .retain(|s| *s != req.subscriber_id);
                    fauna_protocol::encode_canonical(&RemoveSubscriberReply {
                        extra: Default::default(),
                        subscriber: req.subscriber_id,
                        tier: req.tier_name,
                        key_version: 2,
                    })
                    .unwrap()
                    .to_vec()
                }
                "fauna.subscriptions.requests.reject" => {
                    let req: RejectRequestRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode reject");
                    self.reject_calls.lock().unwrap().push(req.request_id);
                    self.pending_requests
                        .lock()
                        .unwrap()
                        .retain(|r| r.request_id != req.request_id);
                    fauna_protocol::encode_canonical(&RejectRequestReply {
                        rejected: true,
                        extra: Default::default(),
                    })
                    .unwrap()
                    .to_vec()
                }
                other => panic!("FakeNest: unexpected kind {other}"),
            };
            Ok(fauna_protocol::decode_strict(&out).expect("decode reply"))
        }
    }

    /// A subscriber actor id that is a VALID Ed25519 public key — the mint does
    /// an X25519 ECDH against each subscriber's key, so a raw `[b; 32]` fill
    /// (rarely a curve point) won't do.
    fn actor(b: u8) -> ActorId {
        ActorKeypair::from_secret([b; 32]).actor_id()
    }

    /// Build an author over a shared fake nest. The keypair seed `0xA0` is the
    /// author identity (distinct from subscriber fills).
    fn author(nest: Arc<FakeNest>) -> SubscriptionsAuthor<Arc<FakeNest>> {
        let keypair = ActorKeypair::from_secret([0xA0; 32]);
        let subs = SubscriptionsClient::new(nest.clone());
        let period_keys = nest.period_keys.shared();
        SubscriptionsAuthor::new(subs, keypair, period_keys)
    }

    /// The author's own identity — the minter every period it records names.
    fn me() -> ActorId {
        ActorKeypair::from_secret([0xA0; 32]).actor_id()
    }

    /// Apply a pure custody transition to the author's custody and persist it
    /// through the store — the join every production write is.
    fn seed_custody(
        a: &SubscriptionsAuthor<Arc<FakeNest>>,
        f: impl FnOnce(&mut SubscriptionsConfig),
    ) -> SubscriptionsConfig {
        let mut replica = held_custody(a);
        f(&mut replica);
        block_on(a.period_keys.merge_custody(replica)).expect("persist custody")
    }

    /// The subscriber-id set a captured upload's `KeyBlob` wraps to.
    fn upload_roster(up: &EncryptedKeyBlobUpload) -> BTreeSet<[u8; 32]> {
        let blob: KeyBlob = decode_signed_bytes(&up.key_blob.bytes).expect("decode minted KeyBlob");
        blob.entries.iter().map(|e| e.subscriber.0).collect()
    }

    fn expect_set(ids: &[ActorId]) -> BTreeSet<[u8; 32]> {
        ids.iter().map(|a| a.0).collect()
    }

    /// [`seed_custody`] as a future over the store's own result — for the
    /// tests that stage custody by hand inside a `block_on`.
    async fn update_custody(
        a: &SubscriptionsAuthor<Arc<FakeNest>>,
        f: impl FnOnce(&mut SubscriptionsConfig),
    ) -> Result<SubscriptionsConfig, StoreError> {
        let mut replica = a.period_keys.custody().await?;
        f(&mut replica);
        a.period_keys.merge_custody(replica).await
    }

    /// The author's custody as the store folds it.
    fn held_custody(author: &SubscriptionsAuthor<Arc<FakeNest>>) -> SubscriptionsConfig {
        block_on(author.period_keys.custody()).expect("read custody")
    }

    // ── the post-succession period-key rotation ─────────────────────────────
    //
    // The predecessor keypair every one of these stages the pre-ceremony world
    // with: `set_authored_key_blob` signs the stored blob as this identity, and
    // `inherited_tier` stamps the custody period `minted_by` it — the stamp the
    // leg actually derives from.
    fn predecessor() -> ActorKeypair {
        ActorKeypair::from_secret([0xD0; 32])
    }

    /// Stage a successor who inherited `tier`: the author holds a period key
    /// (recorded at `rotated_at`) and the nest's stored blob was published by
    /// the PREDECESSOR — the state a ceremony leaves behind.
    fn inherited_tier(
        nest: &Arc<FakeNest>,
        a: &SubscriptionsAuthor<Arc<FakeNest>>,
        tier: &str,
        subscribers: &[ActorId],
        rotated_at: u64,
    ) {
        nest.set_period_rotate(true);
        nest.set_roster(subscribers);
        // Recorded with the PREDECESSOR as the minter: what a ceremony leaves
        // behind is a period the predecessor minted, carried to the successor
        // on the plane — which is the state the leg has to recognise.
        seed_custody(a, |c| {
            custody::record_new_tier(c, predecessor().actor_id(), tier, [0x11; 32], rotated_at);
        });
        nest.set_authored_key_blob(tier, &predecessor(), subscribers, rotated_at, [0x11; 32]);
    }

    #[test]
    /// The property the leg exists for: the key the predecessor's custody held
    /// stops being the one new posts seal under, while every subscriber is
    /// re-wrapped so none of them loses the tier, and the old key is RETAINED
    /// so the back catalogue stays readable (no-user-data-loss).
    fn a_succession_rotates_the_inherited_period_key_and_keeps_the_roster() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let subs = [actor(0xE1), actor(0xE2)];
        inherited_tier(&nest, &a, "gold", &subs, 1_000_000);

        let outcome = block_on(a.rotate_period_keys_after_succession()).expect("rotation");
        assert_eq!(
            outcome,
            PeriodRotationOutcome::Swept {
                rotated: 1,
                already_current: 0,
                failed: 0
            }
        );

        let cfg = held_custody(&a);
        let held = custody::current_period(&cfg, "gold").expect("period");
        assert_eq!(held.version, 2, "a fresh period was minted");
        assert_ne!(*held.key, [0x11; 32], "and it is not the predecessor's key");
        // The back catalogue's key is retained, not burned.
        assert_eq!(cfg.tiers[0].prior.len(), 1);
        assert_eq!(*cfg.tiers[0].prior[0].key, [0x11; 32]);

        // Published to the unchanged roster, under this identity.
        let published = nest.rotated_blobs();
        assert_eq!(published.len(), 1);
        assert_eq!(
            published[0]
                .entries
                .iter()
                .map(|e| e.subscriber.0)
                .collect::<BTreeSet<_>>(),
            expect_set(&subs)
        );
        assert_eq!(published[0].author, a.keypair.actor_id());
    }

    #[test]
    /// Re-runnable at every sign-in, which is what lets the leg be best-effort:
    /// the second pass reads back the blob the first one published, sees its
    /// own identity on it, and rotates nothing. A leg that re-keyed per pass
    /// would churn a new key — and a `prior` entry — at every launch.
    fn a_second_pass_finds_the_stamp_it_planted_and_rotates_nothing() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let subs = [actor(0xE3)];
        inherited_tier(&nest, &a, "gold", &subs, 1_000_000);

        block_on(a.rotate_period_keys_after_succession()).expect("first pass");
        let after_first = custody::current_period(&held_custody(&a), "gold").expect("period");

        let outcome = block_on(a.rotate_period_keys_after_succession()).expect("second pass");
        assert_eq!(
            outcome,
            PeriodRotationOutcome::Swept {
                rotated: 0,
                already_current: 1,
                failed: 0
            }
        );
        assert_eq!(
            custody::current_period(&held_custody(&a), "gold").expect("period"),
            after_first,
            "the second pass left the period untouched"
        );
        assert_eq!(nest.rotated_blobs().len(), 1, "and published nothing");
    }

    #[test]
    /// **The crash between the rotate and the upload.** The fresh key is at
    /// rest before the upload is attempted, so the interrupted pass loses
    /// nothing; the next one must finish the PUBLISH rather than mint a second
    /// key, or one outage would cost a key per sign-in and leave `prior`
    /// carrying periods nothing was ever sealed under.
    fn an_interrupted_upload_republishes_the_same_key_rather_than_minting_another() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let subs = [actor(0xE4)];
        inherited_tier(&nest, &a, "gold", &subs, 1_000_000);

        // Every upload attempt of the first pass dies at the transport.
        *nest.rotate_script.lock().unwrap() =
            vec![Some("fauna.subscriptions.roster_mismatch"); MAX_MINT_ATTEMPTS];
        let outcome = block_on(a.rotate_period_keys_after_succession()).expect("pass runs");
        assert_eq!(
            outcome,
            PeriodRotationOutcome::Swept {
                rotated: 0,
                already_current: 0,
                failed: 1
            }
        );
        let staged = custody::current_period(&held_custody(&a), "gold").expect("period");
        assert_eq!(staged.version, 2, "the fresh key is already at rest");

        // The nest recovers; the next pass finishes the publish.
        nest.rotate_script.lock().unwrap().clear();
        *nest.rotate_calls.lock().unwrap() = 0;
        nest.rotate_uploads.lock().unwrap().clear();
        let outcome = block_on(a.rotate_period_keys_after_succession()).expect("resumed pass");
        assert_eq!(
            outcome,
            PeriodRotationOutcome::Swept {
                rotated: 1,
                already_current: 0,
                failed: 0
            }
        );
        let after = custody::current_period(&held_custody(&a), "gold").expect("period");
        assert_eq!(after.version, 2, "no second key was minted");
        assert_eq!(*after.key, *staged.key);
        assert_eq!(held_custody(&a).tiers[0].prior.len(), 1);
    }

    #[test]
    /// A nest that does not serve the door: the pass must do NOTHING rather
    /// than rotate into a key it cannot publish, and must say so — a silent
    /// no-op would report a clean aftermath over a still-open exposure. The
    /// brake is a data-safety keep under the compat-remnant sweep (a half-run
    /// rotation strands subscribers), never an older-peer arm.
    fn a_nest_without_the_rotate_door_is_a_clean_no_op_that_names_itself() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let subs = [actor(0xE5)];
        inherited_tier(&nest, &a, "gold", &subs, 1_000_000);
        nest.set_period_rotate(false);

        let outcome = block_on(a.rotate_period_keys_after_succession()).expect("pass");
        assert_eq!(outcome, PeriodRotationOutcome::NestCannotRotate);
        assert!(outcome.still_owed(), "the exposure is not retired");
        assert!(outcome.settled_line().is_some(), "and the user is told");
        let held = custody::current_period(&held_custody(&a), "gold").expect("period");
        assert_eq!(held.version, 1, "nothing was rotated");
        assert!(nest.rotated_blobs().is_empty());
    }

    #[test]
    /// An approve that read custody BEFORE a rotation and uploads after it. The nest refuses its first upload as
    /// `stale_rotation` — the rotation's blob is already live — and the retry
    /// must re-read custody and mint under the period that is current NOW.
    /// Bumping `rotated_at` past a publisher it has not read would republish
    /// the pre-rotation key over the rotated blob: here the thief-known
    /// inherited key, served as the tier's live key while the successor seals
    /// new posts under the fresh one.
    fn a_stale_approve_rereads_custody_rather_than_bumping_past_the_rotation() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let member = ActorKeypair::from_secret([0xE6; 32]);
        let joiner = actor(0xE7);
        // Stamped ahead of the wall clock so every `rotated_at` below is
        // `floor + 1` — the collision the nest answers `stale_rotation` to,
        // made deterministic rather than a race against the clock.
        let floor = Timestamp::now().0 + 3_600_000_000;
        inherited_tier(&nest, &a, "gold", &[member.actor_id()], floor);
        let before_rotation = nest.period_keys.snapshot();

        // The leg rotates and publishes: the rotated blob is live on the nest.
        block_on(a.rotate_period_keys_after_succession()).expect("rotation");
        let rotated = custody::current_period(&held_custody(&a), "gold").expect("period");
        assert_eq!(rotated.version, 2);
        let after_rotation = nest.period_keys.snapshot();

        // The approve reads the PRE-rotation custody, and the rotation's config
        // write becomes visible between that read and its upload.
        nest.period_keys.restore(before_rotation);
        *nest.custody_on_approve.lock().unwrap() = Some(after_rotation);
        *nest.approve_keeps_blob.lock().unwrap() = true;
        block_on(a.approve_subscriber(&pending(1, joiner, "gold", SUBSCRIBE_KIND)))
            .expect("the approve lands");

        let live = nest.live_blob("gold").expect("live blob");
        let entry = live
            .entries
            .iter()
            .find(|e| e.subscriber == member.actor_id())
            .expect("the member is still covered");
        assert_eq!(
            decrypt_key_blob_entry_for(&member, entry).expect("open"),
            *rotated.key,
            "the live blob wraps the ROTATED key, not the inherited one"
        );
        assert!(
            live.entries.iter().any(|e| e.subscriber == joiner),
            "and covers the joiner"
        );
    }

    #[test]
    /// The removal twin of the stale approve: a
    /// `stale_rotation` while a removal's rotation is staged but not yet
    /// committed. The live blob may already be the removal's, under a key
    /// custody does not call `current` yet — so the approve can neither bump
    /// past it (the pre-removal key, which the leaver holds, over the rotated
    /// blob) nor mint under the staged key (the removal may not have landed,
    /// and the roster would still carry the leaver). It stands down; the
    /// request stays queued for the next pass.
    fn a_stale_approve_stands_down_while_a_removal_rotation_is_staged() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let leaver = actor(0xE8);
        nest.set_roster(&[leaver]);
        seed_custody(&a, |c| {
            custody::record_new_tier(c, me(), "gold", [0x21; 32], 1_000_000);
        });
        let unstaged = nest.period_keys.snapshot();
        let mut staged = held_custody(&a);
        custody::stage_pending_removal(
            &mut staged,
            PendingRemoval {
                tier_name: "gold".into(),
                subscriber_id: leaver,
                new_period: TierPeriod {
                    version: 2,
                    key: [0x22; 32].into(),
                    rotated_at: 2_000_000,
                    minted_by: Some(a.keypair.actor_id()),
                },
            },
        );
        block_on(a.period_keys.merge_custody(staged)).expect("stage");
        let staged_bytes = nest.period_keys.snapshot();
        nest.period_keys.restore(unstaged);

        *nest.custody_on_approve.lock().unwrap() = Some(staged_bytes);
        *nest.approve_script.lock().unwrap() = vec![Some(STALE_ROTATION)];
        let err = block_on(a.approve_subscriber(&pending(1, actor(0xE9), "gold", SUBSCRIBE_KIND)))
            .expect_err("the approve stands down");
        assert!(
            matches!(err, AuthorError::RotationInFlight { ref tier } if tier == "gold"),
            "got {err}"
        );
        assert_eq!(
            nest.approve_uploads.lock().unwrap().len(),
            1,
            "nothing was uploaded past the staged rotation"
        );
    }

    #[test]
    /// The re-read's stamp floor: a peer device whose clock runs an hour ahead rotated
    /// the tier, so the rotated blob is stamped far past anything this
    /// device's clock or bump would reach. The retry after `stale_rotation`
    /// must climb past the re-read period's own stamp at once; bumping by
    /// `STALE_BUMP_MICROS` alone exhausts the attempts an hour short.
    fn a_stale_approve_climbs_past_a_rotation_stamped_far_ahead_in_one_retry() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let me = &a.keypair;
        let member = actor(0xF1);
        let joiner = actor(0xF2);
        nest.set_roster(&[member]);
        let ahead = Timestamp::now().0 + 3_600_000_000;
        seed_custody(&a, |c| {
            custody::record_new_tier(c, me.actor_id(), "gold", [0x11; 32], 1_000_000);
        });
        let before_rotation = nest.period_keys.snapshot();
        seed_custody(&a, |c| {
            custody::rotate(c, me.actor_id(), "gold", [0x22; 32], ahead).expect("rotate");
        });
        let after_rotation = nest.period_keys.snapshot();
        nest.set_authored_key_blob("gold", me, &[member], ahead, [0x22; 32]);

        nest.period_keys.restore(before_rotation);
        *nest.custody_on_approve.lock().unwrap() = Some(after_rotation);
        *nest.approve_keeps_blob.lock().unwrap() = true;
        block_on(a.approve_subscriber(&pending(1, joiner, "gold", SUBSCRIBE_KIND)))
            .expect("the approve lands");

        assert_eq!(
            nest.approve_uploads.lock().unwrap().len(),
            2,
            "one refusal, then one landing"
        );
        let live = nest.live_blob("gold").expect("live blob");
        assert!(live.rotated_at.0 > ahead);
        assert_eq!(
            live.key_commitment,
            fauna_core::subscription::crypto::period_key_commitment(&[0x22; 32]),
            "the live blob wraps the rotated key"
        );
    }

    #[test]
    /// The approve the nest does NOT refuse: it read custody before the rotation persisted, but its clock
    /// read fell after the rotation computed its stamp, so it lands the
    /// PRE-rotation key with a LATER `rotated_at` than the rotated blob's —
    /// the nest checks only that the stamp advances. Read by freshness the tier
    /// looks settled while the live blob wraps the thief-known key; the next
    /// pass must read the blob's key witness against custody, see a key it
    /// rotated OUT, and republish the current one — without minting another.
    ///
    /// The later clock read is modelled by the stale custody the approve loads
    /// carrying a `rotated_at` no lower than the rotation's, so its stamp is
    /// `floor + 1` past the rotated blob's deterministically rather than by a
    /// race against the wall clock (convention 14).
    fn a_stale_approve_that_outran_the_rotation_is_republished_by_the_next_pass() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let member = ActorKeypair::from_secret([0xEB; 32]);
        let joiner = actor(0xEC);
        let floor = Timestamp::now().0 + 3_600_000_000;
        inherited_tier(&nest, &a, "gold", &[member.actor_id()], floor);
        let before_rotation = held_custody(&a);

        // The leg rotates and publishes: the rotated blob is live on the nest.
        block_on(a.rotate_period_keys_after_succession()).expect("rotation");
        let rotated = custody::current_period(&held_custody(&a), "gold").expect("period");
        assert_eq!(rotated.version, 2);
        let after_rotation = nest.period_keys.snapshot();
        let rotated_live = nest.live_blob("gold").expect("rotated blob");
        assert_eq!(rotated_live.rotated_at.0, rotated.rotated_at);

        // The approve loads the PRE-rotation custody (the inherited key), whose
        // stamp already stands at the rotation's — so its own lands one past
        // the rotated blob's, and the nest has no reason to refuse it.
        let mut stale = before_rotation;
        assert!(custody::restamp_current(
            &mut stale,
            "gold",
            1,
            rotated.rotated_at
        ));
        nest.period_keys.restore_to(&stale);
        *nest.custody_on_approve.lock().unwrap() = Some(after_rotation);
        *nest.approve_keeps_blob.lock().unwrap() = true;
        block_on(a.approve_subscriber(&pending(1, joiner, "gold", SUBSCRIBE_KIND)))
            .expect("the approve lands unrefused");
        assert_eq!(nest.approve_uploads.lock().unwrap().len(), 1, "no retry");
        // Mirror the nest's approve door: the joiner is on the roster now.
        nest.set_roster(&[member.actor_id(), joiner]);

        // The residual, staged as it occurs: the live blob is FRESHER than the
        // rotation yet wraps the inherited key.
        let stale_live = nest.live_blob("gold").expect("live blob");
        assert!(stale_live.rotated_at.0 > rotated.rotated_at);
        let entry = stale_live
            .entries
            .iter()
            .find(|e| e.subscriber == member.actor_id())
            .expect("member entry");
        assert_eq!(
            decrypt_key_blob_entry_for(&member, entry).expect("open"),
            [0x11; 32],
            "the stale approve landed the inherited key over the rotated blob"
        );
        assert_eq!(
            custody::live_blob_key(&held_custody(&a), "gold", &stale_live.key_commitment),
            Some(custody::LiveBlobKey::RotatedOut)
        );

        // The next pass witnesses the KEY, not the stamp: owed, republished.
        nest.rotate_uploads.lock().unwrap().clear();
        let outcome = block_on(a.rotate_period_keys_after_succession()).expect("next pass");
        assert_eq!(
            outcome,
            PeriodRotationOutcome::Swept {
                rotated: 1,
                already_current: 0,
                failed: 0
            },
            "a fresher blob wrapping a rotated-out key is NOT settled"
        );
        let healed = nest.live_blob("gold").expect("healed blob");
        assert!(healed.rotated_at.0 > stale_live.rotated_at.0);
        for who in [member.actor_id(), joiner] {
            assert!(
                healed.entries.iter().any(|e| e.subscriber == who),
                "the republish covers the unchanged roster, joiner included"
            );
        }
        let entry = healed
            .entries
            .iter()
            .find(|e| e.subscriber == member.actor_id())
            .expect("member entry");
        assert_eq!(
            decrypt_key_blob_entry_for(&member, entry).expect("open"),
            *rotated.key,
            "the live blob wraps the ROTATED key again"
        );
        let held = held_custody(&a);
        let current = custody::current_period(&held, "gold").expect("period");
        assert_eq!(current.version, 2, "republished, not minted again");
        assert_eq!(*current.key, *rotated.key);
        assert_eq!(
            current.rotated_at, healed.rotated_at.0,
            "custody carries the winning stamp"
        );
        assert_eq!(held.tiers[0].prior.len(), 1);

        // And a pass after THAT finds the witness matching and rotates nothing.
        let outcome = block_on(a.rotate_period_keys_after_succession()).expect("settled pass");
        assert_eq!(
            outcome,
            PeriodRotationOutcome::Swept {
                rotated: 0,
                already_current: 1,
                failed: 0
            }
        );
    }

    #[test]
    /// The witness's other edge: a live blob wrapping a key this custody has
    /// NEVER held is a peer device's newer rotation that has not merged in
    /// yet — however the stamps compare. Republishing our `current` over it
    /// would be the regression the stale approve commits, from the other
    /// side; the pass leaves it alone and the next config merge classifies it.
    fn a_peer_devices_unmerged_key_is_not_regressed_by_this_custody() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let me = &a.keypair;
        let subs = [actor(0xED)];
        nest.set_period_rotate(true);
        nest.set_roster(&subs);
        block_on(update_custody(&a, |cfg| {
            cfg.tiers.push(fauna_core::data::TierPeriodKeys {
                tier_name: "gold".into(),
                current: TierPeriod {
                    version: 2,
                    key: [0x41; 32].into(),
                    rotated_at: 3_000_000,
                    minted_by: Some(me.actor_id()),
                },
                prior: vec![TierPeriod {
                    version: 1,
                    key: [0x11; 32].into(),
                    rotated_at: 1_000_000,
                    minted_by: Some(me.actor_id()),
                }],
            });
        }))
        .expect("custody");
        // The peer's blob: my authorship, a key I have never seen, and a stamp
        // OLDER than my current's — the freshness rule would call it owed.
        nest.set_authored_key_blob("gold", me, &subs, 2_000_000, [0x77; 32]);
        let before = held_custody(&a);

        let outcome = block_on(a.rotate_period_keys_after_succession()).expect("pass");
        assert_eq!(
            outcome,
            PeriodRotationOutcome::Swept {
                rotated: 0,
                already_current: 1,
                failed: 0
            },
            "a foreign key is not this device's to overwrite"
        );
        assert!(nest.rotated_blobs().is_empty(), "nothing was published");
        assert_eq!(held_custody(&a), before, "and custody is untouched");
    }

    // ── the connect pass: the same witness outside a succession ─────────────

    /// What [`stale_approve_outran_a_removal`] staged.
    struct OutranRemoval {
        member: ActorKeypair,
        joiner: ActorId,
        leaver: ActorId,
        /// The period the removal rotated to — what the live blob owes.
        removed: TierPeriod,
        /// The stale approve's blob, live on the nest.
        stale_live: KeyBlob,
    }

    /// Stage the residual with no succession anywhere: an ordinary removal
    /// rotates the tier and its blob lands, then an approve that read custody
    /// BEFORE the removal committed — but stamped past it — lands the
    /// rotated-out key unrefused. The removal-side mirror of
    /// `a_stale_approve_that_outran_the_rotation_is_republished_by_the_next_pass`,
    /// with the later clock read modelled the same way (convention 14).
    fn stale_approve_outran_a_removal(
        nest: &Arc<FakeNest>,
        a: &SubscriptionsAuthor<Arc<FakeNest>>,
    ) -> OutranRemoval {
        let me = &a.keypair;
        let member = ActorKeypair::from_secret([0xC1; 32]);
        let leaver = actor(0xC2);
        let joiner = actor(0xC3);
        let floor = Timestamp::now().0 + 3_600_000_000;
        nest.set_period_rotate(true);
        nest.set_roster(&[member.actor_id(), leaver]);
        seed_custody(a, |c| {
            custody::record_new_tier(c, me.actor_id(), "gold", [0x11; 32], floor);
        });
        nest.set_authored_key_blob("gold", me, &[member.actor_id(), leaver], floor, [0x11; 32]);
        let before_removal = held_custody(a);

        *nest.remove_keeps_blob.lock().unwrap() = true;
        block_on(a.remove_subscriber("gold", leaver)).expect("removal");
        let removed = custody::current_period(&held_custody(a), "gold").expect("period");
        assert_eq!(removed.version, 2, "the removal rotated");
        let after_removal = nest.period_keys.snapshot();
        assert_eq!(
            nest.live_blob("gold").expect("removal blob").rotated_at.0,
            removed.rotated_at
        );

        let mut stale = before_removal;
        assert!(custody::restamp_current(
            &mut stale,
            "gold",
            1,
            removed.rotated_at
        ));
        nest.period_keys.restore_to(&stale);
        *nest.custody_on_approve.lock().unwrap() = Some(after_removal);
        *nest.approve_keeps_blob.lock().unwrap() = true;
        block_on(a.approve_subscriber(&pending(1, joiner, "gold", SUBSCRIBE_KIND)))
            .expect("the approve lands unrefused");
        nest.set_roster(&[member.actor_id(), joiner]);

        let stale_live = nest.live_blob("gold").expect("live blob");
        let entry = stale_live
            .entries
            .iter()
            .find(|e| e.subscriber == member.actor_id())
            .expect("member entry");
        assert_eq!(
            decrypt_key_blob_entry_for(&member, entry).expect("open"),
            [0x11; 32],
            "the stale approve landed the key the leaver holds"
        );
        assert_eq!(
            custody::live_blob_key(&held_custody(a), "gold", &stale_live.key_commitment),
            Some(custody::LiveBlobKey::RotatedOut)
        );
        OutranRemoval {
            member,
            joiner,
            leaver,
            removed,
            stale_live,
        }
    }

    #[test]
    /// The residual of the key witness: a stale
    /// approve that outran an ORDINARY removal's rotation, where no aftermath
    /// ever runs. The pump's connect pass reads the witness, sees a key custody
    /// rotated out, and republishes `current` over the unchanged roster — the
    /// leaver still off it — without minting another period; later ticks of
    /// the same connection do not ask again.
    fn the_connect_pass_republishes_a_blob_a_stale_approve_keyed_under_a_removed_period() {
        let nest = Arc::new(FakeNest::default());
        let latch = ConnectPassLatch::default();
        let a = author(nest.clone()).with_connect_pass(latch.clone());
        let staged = stale_approve_outran_a_removal(&nest, &a);
        assert!(nest.rotated_blobs().is_empty());

        let pass = block_on(a.reconcile_once());
        assert_eq!(pass.resume_error, None);
        assert_eq!(pass.drain_error, None);
        assert_eq!(nest.rotated_blobs().len(), 1, "one republish");
        let healed = nest.live_blob("gold").expect("healed blob");
        assert!(healed.rotated_at.0 > staged.stale_live.rotated_at.0);
        let entry = healed
            .entries
            .iter()
            .find(|e| e.subscriber == staged.member.actor_id())
            .expect("member entry");
        assert_eq!(
            decrypt_key_blob_entry_for(&staged.member, entry).expect("open"),
            *staged.removed.key,
            "the live blob wraps the post-removal key again"
        );
        assert!(healed.entries.iter().any(|e| e.subscriber == staged.joiner));
        assert!(
            !healed.entries.iter().any(|e| e.subscriber == staged.leaver),
            "the republish covers the post-removal roster"
        );
        let current = custody::current_period(&held_custody(&a), "gold").expect("period");
        assert_eq!(current.version, 2, "republished, not minted again");
        assert_eq!(*current.key, *staged.removed.key);
        assert_eq!(current.rotated_at, healed.rotated_at.0);

        // Later ticks of this connection — and a per-call author over the same
        // connection's latch, the FFI/wasm shape — do not ask again.
        let gets = *nest.key_blob_get_calls.lock().unwrap();
        block_on(a.reconcile_once());
        block_on(
            author(nest.clone())
                .with_connect_pass(latch)
                .reconcile_once(),
        );
        assert_eq!(*nest.key_blob_get_calls.lock().unwrap(), gets);
        // And asked directly, the settled tier publishes nothing.
        let sweep = block_on(a.republish_stale_keyed_blobs()).expect("settled pass");
        assert_eq!(
            sweep,
            StaleKeyedBlobSweep {
                republished: 0,
                failed: 0
            }
        );
        assert_eq!(nest.rotated_blobs().len(), 1);
    }

    #[test]
    /// A connect pass that could not finish a tier does not latch: the next
    /// tick asks again, so one transient fault on connect does not park the
    /// stale key until the next connection.
    fn a_connect_pass_that_failed_a_tier_runs_again_next_tick() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let staged = stale_approve_outran_a_removal(&nest, &a);

        nest.fail_key_blob_get(1);
        block_on(a.reconcile_once());
        assert!(
            nest.rotated_blobs().is_empty(),
            "the faulted tier was skipped"
        );

        block_on(a.reconcile_once());
        assert_eq!(nest.rotated_blobs().len(), 1, "and healed on the next tick");
        let healed = nest.live_blob("gold").expect("healed blob");
        assert_eq!(
            healed.key_commitment,
            fauna_core::subscription::crypto::period_key_commitment(&staged.removed.key)
        );
    }

    #[test]
    /// The connect pass owes exactly `RotatedOut`: a peer device's unmerged
    /// newer key (`Foreign`) is not regressed, a settled tier is left alone,
    /// and a nest without the rotate door is never asked.
    fn the_connect_pass_republishes_only_a_rotated_out_key() {
        let custody_with_prior = |a: &SubscriptionsAuthor<Arc<FakeNest>>| {
            let me = a.keypair.actor_id();
            block_on(update_custody(a, |cfg| {
                cfg.tiers.push(fauna_core::data::TierPeriodKeys {
                    tier_name: "gold".into(),
                    current: TierPeriod {
                        version: 2,
                        key: [0x41; 32].into(),
                        rotated_at: 3_000_000,
                        minted_by: Some(me),
                    },
                    prior: vec![TierPeriod {
                        version: 1,
                        key: [0x11; 32].into(),
                        rotated_at: 1_000_000,
                        minted_by: Some(me),
                    }],
                });
            }))
            .expect("custody");
        };
        let subs = [actor(0xC4)];
        // (live key, blob stamp)
        for (key, stamp) in [
            ([0x77; 32], 2_000_000), // foreign, and older than current
            ([0x41; 32], 3_000_000), // current
        ] {
            let nest = Arc::new(FakeNest::default());
            let a = author(nest.clone());
            let me = &a.keypair;
            nest.set_period_rotate(true);
            nest.set_roster(&subs);
            custody_with_prior(&a);
            nest.set_authored_key_blob("gold", me, &subs, stamp, key);
            let before = held_custody(&a);
            let sweep = block_on(a.republish_stale_keyed_blobs()).expect("pass");
            assert_eq!(sweep.republished, 0, "live key {:#x}", key[0]);
            assert!(nest.rotated_blobs().is_empty());
            assert_eq!(held_custody(&a), before, "custody untouched");
        }

        // No rotate door: the pass does not even read the blob.
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let me = &a.keypair;
        nest.set_roster(&subs);
        custody_with_prior(&a);
        nest.set_authored_key_blob("gold", me, &subs, 2_000_000, [0x11; 32]);
        let sweep = block_on(a.republish_stale_keyed_blobs()).expect("pass");
        assert_eq!(sweep.republished, 0);
        assert_eq!(*nest.key_blob_get_calls.lock().unwrap(), 0);
    }

    #[test]
    /// The republish starts past the stored blob's stamp. A stale-keyed blob stamped by a
    /// device whose clock runs an hour ahead sits far past custody's
    /// `current`. Starting from the period's own stamp, the pass would spend
    /// every attempt `STALE_BUMP_MICROS` at a time and fail. It would fail the
    /// same way on every later pass too, since the stored stamp never moves,
    /// so the tier would never heal. Starting above the stored stamp lands in
    /// one upload.
    fn a_republish_over_a_blob_stamped_far_ahead_lands_in_one_upload() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let me = &a.keypair;
        let subs = [actor(0xF3)];
        nest.set_period_rotate(true);
        nest.set_roster(&subs);
        block_on(update_custody(&a, |cfg| {
            cfg.tiers.push(fauna_core::data::TierPeriodKeys {
                tier_name: "gold".into(),
                current: TierPeriod {
                    version: 2,
                    key: [0x42; 32].into(),
                    rotated_at: 2_000_000,
                    minted_by: Some(me.actor_id()),
                },
                prior: vec![TierPeriod {
                    version: 1,
                    key: [0x11; 32].into(),
                    rotated_at: 1_000_000,
                    minted_by: Some(me.actor_id()),
                }],
            });
        }))
        .expect("custody");
        let ahead = Timestamp::now().0 + 3_600_000_000;
        nest.set_authored_key_blob("gold", me, &subs, ahead, [0x11; 32]);

        let outcome = block_on(a.rotate_period_keys_after_succession()).expect("pass");
        assert_eq!(
            outcome,
            PeriodRotationOutcome::Swept {
                rotated: 1,
                already_current: 0,
                failed: 0
            }
        );
        assert_eq!(nest.rotated_blobs().len(), 1, "no refused round trip");
        let live = nest.live_blob("gold").expect("live blob");
        assert!(live.rotated_at.0 > ahead);
        assert_eq!(
            live.key_commitment,
            fauna_core::subscription::crypto::period_key_commitment(&[0x42; 32])
        );
    }

    #[test]
    /// A period whose minter stamp is ABSENT is owed, even when everything
    /// else about the tier looks current. The field is
    /// optional, so a period can carry none — plain `Option` equality reads
    /// `None` as not-mine. Reading `None` as "mine" would retire the exposure on the
    /// strength of a field a writer merely forgot; reading it as owed costs
    /// one needless rotation.
    fn a_period_with_no_minter_stamp_is_rotated() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let me = &a.keypair;
        let subs = [actor(0xEA)];
        nest.set_period_rotate(true);
        nest.set_roster(&subs);
        block_on(update_custody(&a, |cfg| {
            cfg.tiers.push(fauna_core::data::TierPeriodKeys {
                tier_name: "gold".into(),
                current: TierPeriod {
                    version: 2,
                    key: [0x31; 32].into(),
                    rotated_at: 1_000_000,
                    minted_by: None,
                },
                prior: Vec::new(),
            });
        }))
        .expect("stage");
        // The live blob is this identity's own, at exactly the period's stamp —
        // nothing but the missing minter distinguishes it from a settled tier.
        nest.set_authored_key_blob("gold", me, &subs, 1_000_000, [0x31; 32]);

        assert_eq!(
            block_on(a.rotate_period_keys_after_succession()).expect("pass"),
            PeriodRotationOutcome::Swept {
                rotated: 1,
                already_current: 0,
                failed: 0
            }
        );
        let held = custody::current_period(&held_custody(&a), "gold").expect("period");
        assert_eq!(held.version, 3, "a fresh period was minted");
        assert!(
            held.was_minted_by(&me.actor_id()),
            "and it carries the stamp again"
        );
    }

    #[test]
    /// An identity that never created an encrypted-mode tier — the ordinary
    /// case — pays one custody read and does not even ask the nest what it can
    /// do.
    fn an_author_holding_no_period_key_does_nothing() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        assert_eq!(
            block_on(a.rotate_period_keys_after_succession()).expect("pass"),
            PeriodRotationOutcome::NothingHeld
        );
        assert!(
            block_on(a.rotate_period_keys_after_succession())
                .expect("pass")
                .settled_line()
                .is_none(),
            "and renders no line"
        );
    }

    #[test]
    /// A tier with no stored blob carries no era stamp, so it is treated as
    /// owed: the exposure there is latent (the first approval would mint under
    /// the thief-known key) and the publish is what plants the stamp that stops
    /// the next pass repeating.
    fn an_inherited_tier_with_no_stored_blob_is_still_rotated() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        nest.set_period_rotate(true);
        nest.set_roster(&[]);
        // Inherited, never subscribed to: no stored blob to read anything off,
        // and the exposure is latent rather than absent — the first approval
        // would mint under the thief-known key. Owed-ness comes from custody,
        // so the missing blob costs the leg nothing.
        let mut cfg = held_custody(&a);
        cfg.tiers.push(fauna_core::data::TierPeriodKeys {
            tier_name: "gold".into(),
            current: TierPeriod {
                version: 1,
                key: [0x11; 32].into(),
                rotated_at: 1_000_000,
                minted_by: Some(predecessor().actor_id()),
            },
            prior: Vec::new(),
        });
        block_on(a.period_keys.merge_custody(cfg)).expect("save");

        assert_eq!(
            block_on(a.rotate_period_keys_after_succession()).expect("first"),
            PeriodRotationOutcome::Swept {
                rotated: 1,
                already_current: 0,
                failed: 0
            }
        );
        assert_eq!(nest.rotated_blobs().len(), 1);
        assert!(nest.rotated_blobs()[0].entries.is_empty());
        assert_eq!(
            block_on(a.rotate_period_keys_after_succession()).expect("second"),
            PeriodRotationOutcome::Swept {
                rotated: 0,
                already_current: 1,
                failed: 0
            },
            "the fresh key's own stamp terminates the leg"
        );
    }

    #[test]
    /// **The regression that the author stamp could not carry.** An ordinary approve
    /// republishes the tier's CURRENT key under the caller's authorship, so a
    /// blob can legitimately be authored by the successor while still wrapping
    /// the predecessor's key. The leg used to read that blob's author as "this
    /// tier is already re-keyed" and skip it — silently, for ever, on a
    /// `Swept { failed: 0 }` that reports nothing owed.
    ///
    /// Staged exactly as the defect occurs — the stored blob is authored by
    /// ME and keyed with the PREDECESSOR's key — and the leg must still rotate.
    fn a_successor_authored_blob_over_the_inherited_key_is_still_owed() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let subs = [actor(0xE8)];
        inherited_tier(&nest, &a, "gold", &subs, 1_000_000);
        // The approve: the successor republishes the INHERITED key, so the
        // blob's author becomes me while the key inside is unchanged.
        let me = &a.keypair;
        nest.set_authored_key_blob("gold", me, &subs, 1_500_000, [0x11; 32]);

        let outcome = block_on(a.rotate_period_keys_after_succession()).expect("pass");
        assert_eq!(
            outcome,
            PeriodRotationOutcome::Swept {
                rotated: 1,
                already_current: 0,
                failed: 0
            },
            "a blob this identity published over the predecessor's key is NOT a re-key"
        );
        let held = custody::current_period(&held_custody(&a), "gold").expect("period");
        assert_ne!(
            *held.key, [0x11; 32],
            "the thief's key must not survive an approve that merely re-authored the blob"
        );
        assert_eq!(held.minted_by, Some(me.actor_id()));
    }

    #[test]
    /// The same defect with **no deferral window at all** — the author pump
    /// drains auto-approvals on connect and every 30 s, unsequenced against
    /// this leg, so an approve can land between the config re-key and the
    /// rotation inside the successor's very first session. On the reserved
    /// `followers` tier one arriving follower is enough.
    fn an_approve_racing_the_leg_in_the_same_session_does_not_retire_it() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let subs = [actor(0xE9)];
        inherited_tier(&nest, &a, "followers", &subs, 1_000_000);

        // The race: a follower arrives and the pump approves, republishing the
        // inherited key, before the aftermath's leg gets its turn.
        let joiner = actor(0xEA);
        nest.set_roster(&[subs[0], joiner]);
        let me = &a.keypair;
        nest.set_authored_key_blob("followers", me, &[subs[0], joiner], 1_500_000, [0x11; 32]);

        let outcome = block_on(a.rotate_period_keys_after_succession()).expect("pass");
        assert_eq!(
            outcome,
            PeriodRotationOutcome::Swept {
                rotated: 1,
                already_current: 0,
                failed: 0
            }
        );
        // Both members — the one who was there and the one who just arrived —
        // are covered by the fresh key.
        let published = nest.rotated_blobs();
        assert_eq!(published.len(), 1);
        assert_eq!(
            published[0]
                .entries
                .iter()
                .map(|e| e.subscriber.0)
                .collect::<BTreeSet<_>>(),
            expect_set(&[subs[0], joiner])
        );
        assert_ne!(
            *custody::current_period(&held_custody(&a), "followers")
                .expect("period")
                .key,
            [0x11; 32]
        );
    }

    #[test]
    /// A `key_blob.get` that fails at the transport carries no wire code, and
    /// must NOT be read as "no blob, nothing owed" — that direction looks safe
    /// and silently retires the exposure on an outage.
    fn a_transport_fault_reading_the_stamp_is_a_failure_not_a_skip() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let subs = [actor(0xE6)];
        inherited_tier(&nest, &a, "gold", &subs, 1_000_000);
        nest.fail_key_blob_get(1);

        let outcome = block_on(a.rotate_period_keys_after_succession()).expect("pass");
        assert_eq!(
            outcome,
            PeriodRotationOutcome::Swept {
                rotated: 0,
                already_current: 0,
                failed: 1
            }
        );
        assert!(outcome.still_owed());
        assert!(nest.rotated_blobs().is_empty());
        assert_eq!(
            custody::current_period(&held_custody(&a), "gold")
                .expect("period")
                .version,
            1,
            "and nothing was rotated on a guess"
        );
    }

    #[test]
    /// One unreachable tier must not strand its siblings — the pass is
    /// per-tier best-effort, and the counts say which half landed.
    fn one_failing_tier_does_not_stop_the_others() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let subs = [actor(0xE7)];
        inherited_tier(&nest, &a, "gold", &subs, 1_000_000);
        inherited_tier(&nest, &a, "silver", &subs, 1_000_000);
        // The first tier's stamp read dies; the second's succeeds.
        nest.fail_key_blob_get(1);

        let outcome = block_on(a.rotate_period_keys_after_succession()).expect("pass");
        assert_eq!(
            outcome,
            PeriodRotationOutcome::Swept {
                rotated: 1,
                already_current: 0,
                failed: 1
            }
        );
        assert!(outcome.still_owed(), "the failed tier is still owed");
        assert_eq!(nest.rotated_blobs().len(), 1);
    }

    #[test]
    fn create_tier_records_period_key_and_creates_server_side() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        block_on(a.create_tier("gold", 1, None, None, None, false, None, None, false))
            .expect("create");
        assert_eq!(*nest.tiers_create_calls.lock().unwrap(), 1);
        let cfg = held_custody(&a);
        let period = custody::current_period(&cfg, "gold").expect("period recorded");
        assert_eq!(period.version, 1);
        assert!(cfg.pending_removals.is_empty());
    }

    /// A custody store over a runtime still assembling: a plain read answers
    /// "not running" at once, while a write — and the read a write starts
    /// from — waits the assembly out (`fauna_account_seams::period_keys`),
    /// modelled here as succeeding.
    struct AssemblingRuntime(MemoryPeriodKeyStore);

    #[async_trait::async_trait]
    impl PeriodKeyStore for AssemblingRuntime {
        async fn custody(&self) -> Result<SubscriptionsConfig, StoreError> {
            Err(StoreError::Load(
                "the account runtime is not running".into(),
            ))
        }
        async fn custody_for_write(&self) -> Result<SubscriptionsConfig, StoreError> {
            self.0.custody().await
        }
        async fn merge_custody(
            &self,
            replica: SubscriptionsConfig,
        ) -> Result<SubscriptionsConfig, StoreError> {
            self.0.merge_custody(replica).await
        }
        async fn settle_removal(
            &self,
            removal: PendingRemoval,
        ) -> Result<SubscriptionsConfig, StoreError> {
            self.0.settle_removal(removal).await
        }
    }

    /// Creating a tier is a write: made before the account runtime has
    /// assembled (web's tab, right after sign-in), it waits for the runtime
    /// as its join does, instead of failing on the custody read it starts
    /// from.
    #[test]
    fn create_tier_before_the_runtime_assembles_waits_rather_than_failing() {
        let nest = Arc::new(FakeNest::default());
        let keypair = ActorKeypair::from_secret([0xA0; 32]);
        let store = MemoryPeriodKeyStore::default();
        let a = SubscriptionsAuthor::new(
            SubscriptionsClient::new(nest.clone()),
            keypair,
            Arc::new(AssemblingRuntime(store.clone())),
        );
        block_on(a.create_tier("gold", 1, None, None, None, false, None, None, false))
            .expect("the tier is created once the runtime is up");
        assert_eq!(*nest.tiers_create_calls.lock().unwrap(), 1);
        let cfg = block_on(store.custody()).expect("read custody");
        assert!(custody::current_period(&cfg, "gold").is_some());
    }

    /// The custody is the account plane's: a failure names the period-key
    /// read or write, never the shared `StoreError`'s "user config" seam.
    #[test]
    fn a_period_key_failure_never_names_user_config() {
        let read = AuthorError::<FakeError>::PeriodKeys(StoreError::Load("not running".into()));
        assert_eq!(
            read.to_string(),
            "subscriptions period keys read: not running"
        );
        let write = AuthorError::<FakeError>::PeriodKeys(StoreError::Save("no tip".into()));
        assert_eq!(write.to_string(), "subscriptions period keys write: no tip");
    }

    /// The "sell this post" ordering is only satisfiable because the birth
    /// `KeyBlob`'s content address is computable **locally**, before
    /// `tiers.create` ever runs: the gated post body must name the blob, and the
    /// tier can't be created until the body's hash (the `post_id`) exists.
    /// `stage_tier` must therefore hand back the very same `key_blob_ref` the
    /// nest will store — `blake3(upload.key_blob.bytes)`
    /// (`bins/fauna-nest/src/subscription_handlers.rs:907`).
    #[test]
    fn stage_tier_key_blob_ref_matches_what_the_nest_will_store() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let staged = block_on(a.stage_tier("post-unlock-01")).expect("stage");

        assert_eq!(
            *nest.tiers_create_calls.lock().unwrap(),
            0,
            "staging must not touch the server — the post id isn't known yet"
        );
        assert_eq!(
            staged.key_blob_ref,
            fauna_core::encoding::content_hash(&staged.upload.key_blob.bytes).digest(),
            "key_blob_ref is the nest's own derivation over the staged blob"
        );
        let cfg = held_custody(&a);
        let period = custody::current_period(&cfg, "post-unlock-01").expect("period recorded");
        assert_eq!(
            staged.period_key,
            <[u8; 32]>::from(period.key.clone()),
            "the staged key is the persisted one — a gated body sealed under a \
             throwaway key would be unreadable by every buyer"
        );
    }

    /// `commit_tier` sends the *staged* blob, not a freshly minted one. A second
    /// mint would stamp a new `Timestamp::now()`, so its content address would
    /// differ from the one already baked into the signed post body — every buyer
    /// would then resolve a `key_blob_ref` the nest never stored.
    #[test]
    fn commit_tier_sends_the_staged_blob_and_the_designation() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let staged = block_on(a.stage_tier("post-unlock-02")).expect("stage");
        let staged_bytes = staged.upload.key_blob.bytes.clone();
        let post_id = "11".repeat(32);

        block_on(a.commit_tier(
            "post-unlock-02",
            1,
            None,
            Some("5 EUR".into()),
            None,
            false,
            staged,
            Some(post_id.clone()),
            None,
            false,
        ))
        .expect("commit");

        let reqs = nest.tiers_create_reqs.lock().unwrap();
        assert_eq!(reqs.len(), 1, "one create");
        assert_eq!(
            reqs[0].unlocks_post.as_deref(),
            Some(post_id.as_str()),
            "the designation rides the create — it is create-time immutable, so \
             this is the only call that can ever set it"
        );
        assert_eq!(
            reqs[0].encrypted_upload.key_blob.bytes, staged_bytes,
            "the committed blob is byte-identical to the staged one"
        );
    }

    /// An unlock tier's name must be unique, bounded, and carry no commitment to
    /// the post's sealed text (it renders world-readably on the gated badge).
    #[test]
    fn minted_unlock_tier_names_are_prefixed_bounded_and_distinct() {
        let a = mint_unlock_tier_name();
        let b = mint_unlock_tier_name();
        assert_ne!(a, b, "two mints must not collide");
        for n in [&a, &b] {
            assert!(n.starts_with(UNLOCK_TIER_PREFIX), "{n}");
            assert!(!n.is_empty() && n.len() <= 64, "nest bound is 1..=64: {n}");
            assert!(
                hex::decode(n.trim_start_matches(UNLOCK_TIER_PREFIX)).is_ok(),
                "suffix is hex: {n}"
            );
        }
    }

    /// `create_tier` keeps its exact prior behavior: it is now stage + commit,
    /// and an ordinary tier carries no designation.
    #[test]
    fn create_tier_is_stage_plus_commit_and_designates_nothing() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        block_on(a.create_tier("gold", 1, None, None, None, false, None, None, false))
            .expect("create");

        let reqs = nest.tiers_create_reqs.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].unlocks_post, None, "an ordinary tier sells no post");
        assert!(
            !reqs[0].encrypted_upload.key_blob.bytes.is_empty(),
            "the tier is born carrying its first KeyBlob"
        );
    }

    #[test]
    fn approve_mints_over_post_approval_roster_without_rotating() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        block_on(a.create_tier("gold", 1, None, None, None, false, None, None, false))
            .expect("create");
        nest.set_roster(&[actor(1), actor(2)]);

        let req = PendingRequest {
            extra: Default::default(),
            request_id: 42,
            subscriber_id: actor(3),
            tier_name: "gold".into(),
            kind: "subscribe".into(),
            created_at: Timestamp(0),
            mlkem_encaps_key: None,
            payment_entitled: false,
        };
        block_on(a.approve_subscriber(&req)).expect("approve");

        let uploads = nest.approve_uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1, "one upload");
        assert_eq!(
            upload_roster(&uploads[0]),
            expect_set(&[actor(1), actor(2), actor(3)]),
            "blob covers current roster ∪ new subscriber"
        );
        // No rotation on approve: the period stays version 1.
        let cfg = held_custody(&a);
        assert_eq!(custody::current_period(&cfg, "gold").unwrap().version, 1);
        assert!(cfg.tiers[0].prior.is_empty());
    }

    #[test]
    fn approve_retries_on_roster_mismatch_then_succeeds() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        block_on(a.create_tier("gold", 1, None, None, None, false, None, None, false))
            .expect("create");
        nest.set_roster(&[actor(1)]);
        *nest.approve_script.lock().unwrap() = vec![Some(ROSTER_MISMATCH)];

        let req = PendingRequest {
            extra: Default::default(),
            request_id: 7,
            subscriber_id: actor(2),
            tier_name: "gold".into(),
            kind: "subscribe".into(),
            created_at: Timestamp(0),
            mlkem_encaps_key: None,
            payment_entitled: false,
        };
        block_on(a.approve_subscriber(&req)).expect("approve after retry");
        assert_eq!(*nest.approve_calls.lock().unwrap(), 2, "retried once");
        let uploads = nest.approve_uploads.lock().unwrap();
        assert_eq!(
            upload_roster(uploads.last().unwrap()),
            expect_set(&[actor(1), actor(2)]),
            "re-minted over the re-read roster"
        );
    }

    #[test]
    fn approve_followers_generates_period_key_on_demand() {
        // The nest auto-provisions the reserved `followers` tier, so the author
        // never ran `create_tier` for it and custody holds no period key.
        // Approving the first follow must generate + persist a v1 key on demand
        // and mint over it — not error `NoPeriodKey`.
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        // Precondition: no custody period key for the followers tier.
        assert!(custody::current_period(&held_custody(&a), FOLLOWERS_TIER).is_none());
        nest.set_roster(&[]); // fresh followers tier, empty roster

        let req = PendingRequest {
            extra: Default::default(),
            request_id: 1,
            subscriber_id: actor(5),
            tier_name: FOLLOWERS_TIER.into(),
            kind: "subscribe".into(),
            created_at: Timestamp(0),
            mlkem_encaps_key: None,
            payment_entitled: false,
        };
        block_on(a.approve_subscriber(&req)).expect("approve follow auto-generates the period key");

        // A v1 period key was generated + persisted for the followers tier.
        let period = custody::current_period(&held_custody(&a), FOLLOWERS_TIER)
            .expect("followers period key generated on demand");
        assert_eq!(period.version, 1);
        // The blob wraps the new follower.
        let uploads = nest.approve_uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1, "one upload");
        assert_eq!(upload_roster(&uploads[0]), expect_set(&[actor(5)]));
    }

    #[test]
    fn approve_missing_key_for_non_followers_tier_still_errors() {
        // On-demand generation is scoped to the reserved followers tier only: a
        // missing period key for any *paid* tier is a real bug (a dropped
        // `create_tier` custody write), surfaced as `NoPeriodKey` — never
        // silently healed by minting under a brand-new key that orphans the
        // live roster's existing blob.
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let req = PendingRequest {
            extra: Default::default(),
            request_id: 2,
            subscriber_id: actor(6),
            tier_name: "gold".into(),
            kind: "subscribe".into(),
            created_at: Timestamp(0),
            mlkem_encaps_key: None,
            payment_entitled: false,
        };
        let err = block_on(a.approve_subscriber(&req)).expect_err("must not auto-generate");
        assert!(
            matches!(err, AuthorError::NoPeriodKey(ref t) if t == "gold"),
            "expected NoPeriodKey(gold), got {err:?}"
        );
        assert_eq!(
            *nest.approve_calls.lock().unwrap(),
            0,
            "no upload attempted"
        );
    }

    #[test]
    fn approve_mints_hybrid_when_eks_published() {
        // S4b end-to-end (author side): an existing
        // roster member published an ek and the brand-new subscriber publishes
        // one on the request. The minted blob is X-Wing for BOTH, and each opens
        // to the tier's period key via the unified read dispatcher.
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        block_on(a.create_tier("gold", 1, None, None, None, false, None, None, false))
            .expect("create");

        let existing = ActorKeypair::from_secret([0x11; 32]);
        let newcomer = ActorKeypair::from_secret([0x12; 32]);
        nest.set_roster(&[existing.actor_id()]);
        nest.set_subscriber_ek(
            existing.actor_id(),
            subscriber_mlkem_encaps_key(&existing).to_vec(),
        );

        let req = PendingRequest {
            extra: Default::default(),
            request_id: 99,
            subscriber_id: newcomer.actor_id(),
            tier_name: "gold".into(),
            kind: "subscribe".into(),
            created_at: Timestamp(0),
            mlkem_encaps_key: Some(ByteBuf::from(
                subscriber_mlkem_encaps_key(&newcomer).to_vec(),
            )),
            payment_entitled: false,
        };
        block_on(a.approve_subscriber(&req)).expect("approve hybrid");

        let period_key = custody::current_period(&held_custody(&a), "gold")
            .expect("period")
            .key;
        let uploads = nest.approve_uploads.lock().unwrap();
        let blob: KeyBlob =
            decode_signed_bytes(&uploads[0].key_blob.bytes).expect("decode minted KeyBlob");
        assert_eq!(blob.entries.len(), 2);
        // Every entry is X-Wing (both members published an ek).
        assert!(blob.entries.iter().all(|e| e.suite == KemSuiteId::Xwing));
        for sub in [&existing, &newcomer] {
            let entry = blob
                .entries
                .iter()
                .find(|e| e.subscriber == sub.actor_id())
                .expect("entry for subscriber");
            assert_eq!(
                decrypt_key_blob_entry_for(sub, entry).expect("hybrid open"),
                period_key,
                "X-Wing entry opens to the period key"
            );
        }
    }

    #[test]
    fn remove_rotates_mints_post_removal_roster_and_commits() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        block_on(a.create_tier("gold", 1, None, None, None, false, None, None, false))
            .expect("create");
        let v1_key = held_custody(&a).tiers[0].current.key.clone();
        nest.set_roster(&[actor(1), actor(2)]);

        block_on(a.remove_subscriber("gold", actor(2))).expect("remove");

        let uploads = nest.remove_uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1);
        assert_eq!(
            upload_roster(&uploads[0]),
            expect_set(&[actor(1)]),
            "blob covers roster MINUS the removed subscriber"
        );
        let cfg = held_custody(&a);
        let tier = &cfg.tiers[0];
        assert_eq!(tier.current.version, 2, "rotated to v2");
        assert_ne!(tier.current.key, v1_key, "fresh post-removal key");
        assert_eq!(tier.prior.len(), 1, "v1 retained for archival backfill");
        assert_eq!(tier.prior[0].key, v1_key);
        assert!(
            cfg.pending_removals.is_empty(),
            "sentinel cleared after commit"
        );
        assert_eq!(nest.roster.lock().unwrap().as_slice(), &[actor(1)]);
    }

    #[test]
    fn remove_retries_on_stale_rotation_then_commits() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        block_on(a.create_tier("gold", 1, None, None, None, false, None, None, false))
            .expect("create");
        nest.set_roster(&[actor(1), actor(2)]);
        *nest.remove_script.lock().unwrap() = vec![Some(STALE_ROTATION)];

        block_on(a.remove_subscriber("gold", actor(2))).expect("remove after retry");
        assert_eq!(*nest.remove_calls.lock().unwrap(), 2, "retried once");
        let cfg = held_custody(&a);
        assert_eq!(custody::current_period(&cfg, "gold").unwrap().version, 2);
        assert!(cfg.pending_removals.is_empty());
    }

    #[test]
    fn remove_absent_subscriber_is_a_noop_without_rotating() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        block_on(a.create_tier("gold", 1, None, None, None, false, None, None, false))
            .expect("create");
        nest.set_roster(&[actor(1)]);

        block_on(a.remove_subscriber("gold", actor(9))).expect("noop");
        assert_eq!(
            *nest.remove_calls.lock().unwrap(),
            0,
            "no upload for a noop"
        );
        let cfg = held_custody(&a);
        assert_eq!(
            custody::current_period(&cfg, "gold").unwrap().version,
            1,
            "no pointless rotation"
        );
    }

    #[test]
    fn resume_drives_a_staged_removal_with_the_irrecoverable_key() {
        // Simulate a crash AFTER staging the fresh key but BEFORE the upload:
        // stage + persist the sentinel by hand, then resume.
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        block_on(a.create_tier("gold", 1, None, None, None, false, None, None, false))
            .expect("create");
        nest.set_roster(&[actor(1), actor(2)]);

        let staged_key = [0x5A; 32];
        let mut cfg = held_custody(&a);
        custody::stage_pending_removal(
            &mut cfg,
            PendingRemoval {
                tier_name: "gold".into(),
                subscriber_id: actor(2),
                new_period: TierPeriod {
                    version: 2,
                    key: staged_key.into(),
                    rotated_at: held_custody(&a).tiers[0].current.rotated_at + 1,
                    minted_by: None,
                },
            },
        );
        block_on(a.period_keys.merge_custody(cfg)).expect("persist sentinel");

        let resumed = block_on(a.resume_pending_removals()).expect("resume");
        assert_eq!(resumed, 1);

        let uploads = nest.remove_uploads.lock().unwrap();
        assert_eq!(
            upload_roster(uploads.last().unwrap()),
            expect_set(&[actor(1)])
        );
        let cfg = held_custody(&a);
        // The committed current MUST be the staged (irrecoverable) key — not a
        // freshly-regenerated one — proving the staged key survived the "crash".
        assert_eq!(
            custody::current_period(&cfg, "gold").unwrap().key,
            staged_key
        );
        assert_eq!(custody::current_period(&cfg, "gold").unwrap().version, 2);
        assert!(cfg.pending_removals.is_empty());
    }

    #[test]
    fn resume_treats_already_applied_removal_as_committed() {
        // Simulate a crash AFTER the upload succeeded (subscriber already gone on
        // nest) but BEFORE the local commit: the staged sentinel remains, the
        // nest rejects the re-upload `not_subscribed`, and resume must still
        // commit the staged key locally (idempotent), not error.
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        block_on(a.create_tier("gold", 1, None, None, None, false, None, None, false))
            .expect("create");
        nest.set_roster(&[actor(1)]); // sub(2) already removed by the crashed upload
        *nest.remove_script.lock().unwrap() = vec![Some(NOT_SUBSCRIBED)];

        let staged_key = [0x7C; 32];
        let mut cfg = held_custody(&a);
        let base_rotated = cfg.tiers[0].current.rotated_at + 1;
        custody::stage_pending_removal(
            &mut cfg,
            PendingRemoval {
                tier_name: "gold".into(),
                subscriber_id: actor(2),
                new_period: TierPeriod {
                    version: 2,
                    key: staged_key.into(),
                    rotated_at: base_rotated,
                    minted_by: None,
                },
            },
        );
        block_on(a.period_keys.merge_custody(cfg)).expect("persist sentinel");

        assert_eq!(block_on(a.resume_pending_removals()).expect("resume"), 1);
        let cfg = held_custody(&a);
        assert_eq!(
            custody::current_period(&cfg, "gold").unwrap().key,
            staged_key
        );
        assert!(cfg.pending_removals.is_empty());
    }

    // ── drain_auto_approvals ─────────────────────────────────────────────────

    /// A pending `requests.list` row (`drain_auto_approvals` iterates these).
    fn pending(request_id: i64, subscriber: ActorId, tier: &str, kind: &str) -> PendingRequest {
        PendingRequest {
            extra: Default::default(),
            request_id,
            subscriber_id: subscriber,
            tier_name: tier.into(),
            kind: kind.into(),
            created_at: Timestamp(0),
            mlkem_encaps_key: None,
            payment_entitled: false,
        }
    }

    /// A payment-entitled pending row (Pillar 3: verified payment on a
    /// non-auto tier — the drain approves it without creator judgment).
    fn pending_paid(request_id: i64, subscriber: ActorId, tier: &str) -> PendingRequest {
        PendingRequest {
            payment_entitled: true,
            ..pending(request_id, subscriber, tier, "subscribe")
        }
    }

    /// A `tiers.list` row carrying the `auto_approve` flag the drain resolves.
    fn tier_item(name: &str, rank: u32, auto_approve: bool) -> TierItem {
        TierItem {
            name: name.into(),
            rank,
            description: None,
            price_hint: None,
            payment_url: None,
            auto_approve,
            created_at: Timestamp(0),
            unlocks_post: None,
            asking_price: None,
            hidden: false,
            extra: Default::default(),
        }
    }

    /// [`tier_item`] with `hidden: true` — the reserved owner-only tier's
    /// shape (monetization.md § The unifying model → *A tier may be
    /// hidden*), or any other creator-hidden tier.
    fn hidden_tier_item(name: &str, rank: u32, auto_approve: bool) -> TierItem {
        TierItem {
            hidden: true,
            ..tier_item(name, rank, auto_approve)
        }
    }

    #[test]
    fn drain_approves_auto_subscribes_and_commits_unsubscribes() {
        // Mixed queue: followers (auto) + gold (paid, auto) get approved; silver
        // (paid, NOT auto) stays pending; a followers *unsubscribe* is COMMITTED
        // (a leave needs no judgment — monetization.md § Pillar 1) and its queue
        // row cleared. The leaver is not on the roster here, so the removal is a
        // no-op rotation-wise; the commit still clears the row.
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        // The paid tiers were author-created (custody keys exist); followers has
        // no custody key (nest auto-provisions it) — the drain must still mint it.
        block_on(a.create_tier("gold", 1, None, None, None, true, None, None, false))
            .expect("create gold");
        block_on(a.create_tier("silver", 2, None, None, None, false, None, None, false))
            .expect("create silver");
        nest.set_tiers(&[
            tier_item(FOLLOWERS_TIER, 0, true),
            tier_item("gold", 1, true),
            tier_item("silver", 2, false),
        ]);
        nest.set_roster(&[]); // empty roster for each per-tier subscribers.list
        nest.set_requests(&[
            pending(1, actor(5), FOLLOWERS_TIER, "subscribe"), // ✓ auto follow
            pending(2, actor(6), "gold", "subscribe"),         // ✓ auto paid
            pending(3, actor(7), "silver", "subscribe"),       // ✗ not auto-approve
            pending(4, actor(8), FOLLOWERS_TIER, "unsubscribe"), // ✓ committed, no judgment
        ]);

        let n = block_on(a.drain_auto_approvals()).expect("drain");
        assert_eq!(
            n, 3,
            "two auto-approve subscribes + one committed unsubscribe"
        );
        assert_eq!(
            nest.approve_uploads.lock().unwrap().len(),
            2,
            "one mint per auto-approved subscribe; silver minted nothing"
        );
        assert_eq!(
            nest.remove_uploads.lock().unwrap().len(),
            0,
            "the leaver was not on the roster, so the removal was a no-op"
        );
        assert_eq!(
            *nest.reject_calls.lock().unwrap(),
            vec![4],
            "the committed unsubscribe's queue row was cleared"
        );
        // The followers follow generated its period key on demand (no create_tier
        // ran for the reserved tier).
        assert!(custody::current_period(&held_custody(&a), FOLLOWERS_TIER).is_some());
    }

    #[test]
    fn drain_commits_an_unsubscribe_via_the_removal_rotation() {
        // The leaver IS on the roster: the commit runs the full removal rotation
        // (fresh period key staged, KeyBlob over the post-removal roster uploaded
        // via `subscribers.remove`) and then clears the queue row.
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        block_on(a.create_tier("gold", 1, None, None, None, false, None, None, false))
            .expect("create gold");
        nest.set_tiers(&[tier_item("gold", 1, false)]);
        nest.set_roster(&[actor(8)]);
        nest.set_requests(&[pending(7, actor(8), "gold", "unsubscribe")]);

        let n = block_on(a.drain_auto_approvals()).expect("drain");
        assert_eq!(n, 1, "the unsubscribe committed");
        assert_eq!(
            nest.remove_uploads.lock().unwrap().len(),
            1,
            "the removal rotation uploaded a post-removal KeyBlob"
        );
        assert_eq!(
            *nest.reject_calls.lock().unwrap(),
            vec![7],
            "the committed unsubscribe's queue row was cleared"
        );
        assert!(
            nest.roster.lock().unwrap().is_empty(),
            "the confirmed removal dropped the leaver"
        );
    }

    #[test]
    fn approve_subscriber_refuses_a_non_subscribe_request() {
        // The approve handler discards the row's `kind`, so feeding it an
        // `unsubscribe` row would re-add the leaver — refused at the seam.
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        block_on(a.create_tier("gold", 1, None, None, None, false, None, None, false))
            .expect("create gold");

        let err = block_on(a.approve_subscriber(&pending(9, actor(3), "gold", "unsubscribe")))
            .expect_err("an unsubscribe row must never be approve-minted");
        assert!(
            matches!(&err, AuthorError::WrongRequestKind { kind } if kind == "unsubscribe"),
            "expected WrongRequestKind, got {err:?}"
        );
        assert!(
            nest.approve_uploads.lock().unwrap().is_empty(),
            "no mint may have happened"
        );
    }

    #[test]
    fn approve_subscriber_refuses_a_hidden_tier_request() {
        // Ruling 4 (`monetization.md` § The unifying model → *A tier may be
        // hidden*): hidden means not offered and not subscribable, so no
        // approve-mint may cover it — checked directly here, independent of
        // the drain's own skip, so removing JUST this guard still reds a
        // pin (see [`SubscriptionsAuthor::approve_subscriber`]'s doc for why
        // both guards exist).
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        nest.set_tiers(&[hidden_tier_item("backstage", 5, false)]);

        let err = block_on(a.approve_subscriber(&pending(9, actor(3), "backstage", "subscribe")))
            .expect_err("a hidden tier must never be approve-minted");
        assert!(
            matches!(&err, AuthorError::HiddenTierRequest { tier } if tier == "backstage"),
            "expected HiddenTierRequest, got {err:?}"
        );
        assert!(
            nest.approve_uploads.lock().unwrap().is_empty(),
            "no mint may have happened"
        );
    }

    #[test]
    fn drain_skips_a_lone_hidden_tier_request_with_no_error_surfaced() {
        // A LONE hidden request (nothing else to drain) proves the drain's
        // OWN skip, not `approve_subscriber`'s refusal surfacing as an
        // error: if the drain relied on the mint-time refusal alone, this
        // would return `Err(HiddenTierRequest)` (the `committed == 0` arm of
        // the first-error rule) instead of `Ok(0)` — removing the
        // drain-side skip must red this. `payment_entitled` is set because
        // it is what otherwise bypasses `auto_approve`; the guard must stop
        // it too.
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        nest.set_tiers(&[hidden_tier_item("backstage", 5, false)]);
        nest.set_requests(&[pending_paid(1, actor(5), "backstage")]);

        let n = block_on(a.drain_auto_approvals())
            .expect("a hidden-tier request must not surface as a drain error");
        assert_eq!(n, 0, "the hidden-tier request drains nothing");
        assert!(
            nest.approve_uploads.lock().unwrap().is_empty(),
            "no mint may cover a hidden tier's subscriber"
        );
    }

    #[test]
    fn drain_approves_the_followers_self_heal_beside_a_skipped_hidden_request() {
        // Second positive control alongside the hidden-tier skip (a
        // dedicated verify-back track grades this build): the guard must
        // not regress the reserved FOLLOWERS_TIER's
        // first-approval self-heal — no `create_tier` ever ran for it, so
        // `approve_subscriber`'s own `FOLLOWERS_TIER` branch must still mint
        // the custody key on demand while a hidden request drains beside it.
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        nest.set_tiers(&[
            hidden_tier_item("backstage", 5, false),
            tier_item(FOLLOWERS_TIER, 0, true),
        ]);
        nest.set_requests(&[
            pending_paid(1, actor(6), "backstage"), // ✗ hidden, skipped
            pending(2, actor(5), FOLLOWERS_TIER, "subscribe"), // ✓ first-ever follow
        ]);

        let n = block_on(a.drain_auto_approvals()).expect("drain");
        assert_eq!(
            n, 1,
            "only the follow drains; the hidden request is skipped silently"
        );
        let uploads = nest.approve_uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1, "one mint for the follower");
        assert_eq!(upload_roster(&uploads[0]), expect_set(&[actor(5)]));
        assert!(
            custody::current_period(&held_custody(&a), FOLLOWERS_TIER).is_some(),
            "the follow self-healed a custody period key"
        );
    }

    #[test]
    fn drain_approves_a_payment_entitled_request_on_a_non_auto_tier() {
        // Pillar 3: a verified payment is the third grant source — a
        // `payment_entitled` request on a paid, NOT-auto tier is minted by
        // the pump exactly like an auto-approval, while an unpaid request on
        // the same tier stays pending for the author.
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        block_on(a.create_tier("gold", 1, None, None, None, false, None, None, false))
            .expect("create gold");
        nest.set_tiers(&[tier_item("gold", 1, false)]);
        nest.set_roster(&[]);
        nest.set_requests(&[
            pending_paid(1, actor(5), "gold"),         // ✓ verified payment
            pending(2, actor(6), "gold", "subscribe"), // ✗ unpaid, not auto
        ]);

        let n = block_on(a.drain_auto_approvals()).expect("drain");
        assert_eq!(n, 1, "only the payment-entitled request drains");
        let uploads = nest.approve_uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1, "one mint for the paid subscriber");
        assert_eq!(
            upload_roster(&uploads[0]),
            expect_set(&[actor(5)]),
            "the mint covers the paying subscriber"
        );
    }

    #[test]
    fn drain_with_no_requests_is_a_noop() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        // No requests queued (default empty) — returns 0 without minting.
        assert_eq!(block_on(a.drain_auto_approvals()).expect("drain"), 0);
        assert!(nest.approve_uploads.lock().unwrap().is_empty());
    }

    #[test]
    fn drain_never_approves_an_unsubscribe_even_for_an_auto_tier() {
        // An `unsubscribe` row for an auto_approve tier must never be
        // approve-minted — the approve handler discards the row's kind, so
        // feeding it to approve would re-add the leaver. It is COMMITTED via
        // the removal path instead (here a no-op: the leaver is off the
        // roster), and its queue row cleared.
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        nest.set_tiers(&[tier_item(FOLLOWERS_TIER, 0, true)]);
        nest.set_roster(&[]);
        nest.set_requests(&[pending(1, actor(5), FOLLOWERS_TIER, "unsubscribe")]);

        assert_eq!(
            block_on(a.drain_auto_approvals()).expect("drain"),
            1,
            "the unsubscribe committed"
        );
        assert!(
            nest.approve_uploads.lock().unwrap().is_empty(),
            "an unsubscribe row must never reach approve (would re-add the leaver)"
        );
        assert_eq!(
            *nest.reject_calls.lock().unwrap(),
            vec![1],
            "the committed unsubscribe's queue row was cleared"
        );
    }

    #[test]
    fn drain_skips_a_poisoned_request_and_still_approves_the_rest() {
        // "gold" is auto_approve but the author holds NO custody key for it (a
        // dropped create_tier custody write) → approve errors NoPeriodKey. The
        // followers follow queued alongside it must still be approved.
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        nest.set_tiers(&[
            tier_item("gold", 1, true), // auto, but no custody key → poison
            tier_item(FOLLOWERS_TIER, 0, true),
        ]);
        nest.set_roster(&[]);
        nest.set_requests(&[
            pending(1, actor(6), "gold", "subscribe"), // poison (NoPeriodKey)
            pending(2, actor(5), FOLLOWERS_TIER, "subscribe"), // must still approve
        ]);

        let n = block_on(a.drain_auto_approvals()).expect("partial success returns Ok");
        assert_eq!(n, 1, "the followers follow was approved despite the poison");
        let uploads = nest.approve_uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1);
        assert_eq!(upload_roster(&uploads[0]), expect_set(&[actor(5)]));
    }

    #[test]
    fn drain_surfaces_the_error_when_nothing_drains() {
        // A lone poisoned auto tier and no healthy request: surface the failure
        // (approved == 0) so the caller can log it, not silently return Ok(0).
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        nest.set_tiers(&[tier_item("gold", 1, true)]);
        nest.set_requests(&[pending(1, actor(6), "gold", "subscribe")]);

        let err = block_on(a.drain_auto_approvals()).expect_err("all-poison surfaces");
        assert!(
            matches!(err, AuthorError::NoPeriodKey(ref t) if t == "gold"),
            "expected NoPeriodKey(gold), got {err:?}"
        );
    }

    // ── reconcile_once — the author pump's whole tick body ───────────────────

    /// The load-bearing ORDER assertion, pinned through an observable
    /// consequence rather than a call spy: resume must run **before** drain, so
    /// the drain's mint covers the post-removal roster. If the two ever swap (or
    /// resume is hoisted out of the tick, as the windows pump had it), the
    /// drain mints a `KeyBlob` that still wraps to the subscriber being removed
    /// — i.e. the removed subscriber keeps read access for a full period.
    #[test]
    fn reconcile_once_resumes_before_it_drains() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        block_on(a.create_tier("gold", 1, None, None, None, true, None, None, false))
            .expect("create gold");
        nest.set_tiers(&[tier_item("gold", 1, true)]);
        // actor(2) is mid-removal (staged, upload interrupted); actor(5) is a
        // queued auto-approve follow. Both are pending in the SAME tick.
        nest.set_roster(&[actor(1), actor(2)]);
        nest.set_requests(&[pending(1, actor(5), "gold", "subscribe")]);

        let mut cfg = held_custody(&a);
        let rotated = cfg.tiers[0].current.rotated_at + 1;
        custody::stage_pending_removal(
            &mut cfg,
            PendingRemoval {
                tier_name: "gold".into(),
                subscriber_id: actor(2),
                new_period: TierPeriod {
                    version: 2,
                    key: [0x5A; 32].into(),
                    rotated_at: rotated,
                    minted_by: None,
                },
            },
        );
        block_on(a.period_keys.merge_custody(cfg)).expect("persist sentinel");

        let pass = block_on(a.reconcile_once());

        assert_eq!(pass.resumed, 1, "the staged removal healed in this tick");
        assert_eq!(
            pass.approved, 1,
            "the queued follow drained in the SAME tick"
        );
        assert_eq!(pass.resume_error, None);
        assert_eq!(pass.drain_error, None);

        // The proof of ordering: the approve mint must NOT wrap to actor(2).
        let uploads = nest.approve_uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1, "one mint for the approved follow");
        assert_eq!(
            upload_roster(&uploads[0]),
            expect_set(&[actor(1), actor(5)]),
            "the drain minted over the POST-removal roster — actor(2) is gone, \
             which is only true if resume ran first in the same tick"
        );
    }

    /// Best-effort: a failing resume must not cost the tick its drain. Each half
    /// reports its own error so a shell logs both, and neither can abort the
    /// other (the pumps all swallow per call today — this makes that the shared
    /// contract instead of six independent try/catch pairs).
    #[test]
    fn reconcile_once_still_drains_when_resume_fails() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        block_on(a.create_tier("gold", 1, None, None, None, true, None, None, false))
            .expect("create gold");
        nest.set_tiers(&[tier_item("gold", 1, true)]);
        nest.set_roster(&[actor(1), actor(2)]);
        nest.set_requests(&[pending(1, actor(5), "gold", "subscribe")]);
        // Every remove attempt is rejected with a retryable code, so the staged
        // removal exhausts MAX_MINT_ATTEMPTS and resume returns an error.
        *nest.remove_script.lock().unwrap() = vec![Some(ROSTER_MISMATCH); MAX_MINT_ATTEMPTS + 1];

        let mut cfg = held_custody(&a);
        let rotated = cfg.tiers[0].current.rotated_at + 1;
        custody::stage_pending_removal(
            &mut cfg,
            PendingRemoval {
                tier_name: "gold".into(),
                subscriber_id: actor(2),
                new_period: TierPeriod {
                    version: 2,
                    key: [0x5A; 32].into(),
                    rotated_at: rotated,
                    minted_by: None,
                },
            },
        );
        block_on(a.period_keys.merge_custody(cfg)).expect("persist sentinel");

        let pass = block_on(a.reconcile_once());

        assert_eq!(pass.resumed, 0);
        assert!(
            pass.resume_error.is_some(),
            "the exhausted removal is reported, not swallowed"
        );
        assert_eq!(
            pass.approved, 1,
            "the drain still ran despite resume failing"
        );
        assert_eq!(pass.drain_error, None);
    }

    /// A quiet tick costs two reads and reports nothing — the overwhelmingly
    /// common case on the 30 s backstop.
    #[test]
    fn reconcile_once_with_nothing_pending_is_a_silent_noop() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        let pass = block_on(a.reconcile_once());
        assert_eq!(pass, ReconcilePass::default());
        assert!(nest.approve_uploads.lock().unwrap().is_empty());
        assert!(nest.remove_uploads.lock().unwrap().is_empty());
    }

    // ── the backstop cadence policy ──────────────────────────────────────────

    /// The classifier is pure so the policy is testable without touching
    /// process-global env (which is racy under the test harness' threads, and
    /// `unsafe` since the 2024 edition). `author_poll_secs()` is the thin native
    /// probe over it — the locale-week-start shape.
    #[test]
    fn poll_secs_rejects_every_unusable_override_and_falls_back() {
        assert_eq!(poll_secs_from_raw(Some("2")), 2, "the e2e fast cadence");
        assert_eq!(poll_secs_from_raw(Some("600")), 600);
        assert_eq!(poll_secs_from_raw(None), DEFAULT_AUTHOR_POLL_SECS);
        // A zero would busy-spin the pump against the nest; a negative, a float
        // and a word are all unparseable. Every one falls back rather than
        // producing a pathological cadence.
        assert_eq!(poll_secs_from_raw(Some("0")), DEFAULT_AUTHOR_POLL_SECS);
        assert_eq!(poll_secs_from_raw(Some("-5")), DEFAULT_AUTHOR_POLL_SECS);
        assert_eq!(poll_secs_from_raw(Some("1.5")), DEFAULT_AUTHOR_POLL_SECS);
        assert_eq!(poll_secs_from_raw(Some("")), DEFAULT_AUTHOR_POLL_SECS);
        assert_eq!(poll_secs_from_raw(Some("soon")), DEFAULT_AUTHOR_POLL_SECS);
    }

    #[test]
    fn the_default_cadence_is_the_one_the_goal_doc_names() {
        // `monetization.md` § Pillar 1 matrix row: "drain_auto_approvals on
        // connect + 30 s backstop".
        assert_eq!(DEFAULT_AUTHOR_POLL_SECS, 30);
        assert_eq!(author_poll_interval(), core::time::Duration::from_secs(30));
    }

    // ── run_author_reconcile_loop — the shared linux/tui loop body ───────────

    /// The loop must stop the moment a newer login bumps `generation` — never
    /// ride out one more tick. The injected `sleep` doubles as the "end of
    /// tick N" hook (it runs after every `reconcile_once`, exactly where a
    /// newer login would race in), so bumping the generation there and
    /// counting how many times `sleep` itself got called pins both halves at
    /// once: the loop ran exactly 3 ticks, and the 4th generation check
    /// caught the bump instead of a 4th tick sneaking through.
    #[test]
    fn run_author_reconcile_loop_stops_once_a_newer_login_supersedes_it() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest);
        let generation = AtomicU64::new(1);
        let sleep_calls = std::sync::atomic::AtomicUsize::new(0);

        block_on(run_author_reconcile_loop(&a, &generation, 1, |_| async {
            let n = sleep_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if n == 3 {
                // A newer login superseded this loop mid-tick — the shape
                // the e2e session drivers' repeated re-auth needs.
                generation.store(2, Ordering::SeqCst);
            }
        }));

        assert_eq!(
            sleep_calls.load(Ordering::SeqCst),
            3,
            "the 4th generation check must catch the bump before a 4th tick"
        );
    }

    /// A generation that is already stale when the loop starts — the caller's
    /// own login was itself instantly superseded — must tick **zero** times,
    /// not one. The check runs before the first `reconcile_once`.
    #[test]
    fn run_author_reconcile_loop_with_a_stale_generation_never_ticks() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest);
        let generation = AtomicU64::new(2);
        let sleep_calls = std::sync::atomic::AtomicUsize::new(0);

        block_on(run_author_reconcile_loop(&a, &generation, 1, |_| async {
            sleep_calls.fetch_add(1, Ordering::SeqCst);
        }));

        assert_eq!(sleep_calls.load(Ordering::SeqCst), 0);
    }

    // ── the reserved-tier provisioning (archive-import slice 3) ────────

    /// `provision_owner_only_tier`: first call stages + commits the reserved
    /// tier hidden/unapproved at the top rank and returns material whose
    /// `key_blob_ref` is the birth blob's address; the second call finds the
    /// row on `tiers.list` and mints nothing.
    #[test]
    fn provision_owner_only_tier_mints_once_then_finds_the_row() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());

        let material = block_on(a.provision_owner_only_tier()).expect("provision");

        {
            let reqs = nest.tiers_create_reqs.lock().unwrap();
            assert_eq!(reqs.len(), 1, "the reserved tier is minted exactly once");
            assert_eq!(reqs[0].name, OWNER_ONLY_TIER);
            assert_eq!(reqs[0].rank, OWNER_ONLY_TIER_RANK);
            assert!(reqs[0].hidden, "never offered");
            assert!(
                !reqs[0].auto_approve,
                "belt and braces: nothing may ever be granted"
            );
        }

        // The material is the author's own custody key, not a fresh throwaway,
        // and the blob address is the one the nest actually stored.
        let cfg = held_custody(&a);
        let period = custody::current_period(&cfg, OWNER_ONLY_TIER).expect("period recorded");
        assert_eq!(*material.period_key, *period.key);
        assert_eq!(material.period_version, period.version);
        assert_eq!(material.tier, OWNER_ONLY_TIER);
        assert_eq!(material.rank, OWNER_ONLY_TIER_RANK);
        assert_eq!(
            material.key_blob_ref,
            nest.key_blobs.lock().unwrap()[OWNER_ONLY_TIER].1
        );

        // Second call: the row is on `tiers.list` now, so nothing is minted.
        let again = block_on(a.provision_owner_only_tier()).expect("idempotent");
        assert_eq!(
            nest.tiers_create_reqs.lock().unwrap().len(),
            1,
            "an existing reserved row is reused, never re-created"
        );
        assert_eq!(again.key_blob_ref, material.key_blob_ref);
        assert_eq!(*again.period_key, *material.period_key);
    }

    /// `provision_followers_tier` on an account nobody follows: records a
    /// period key in custody (the approve-time arm's shape), creates the
    /// reserved row at rank 0 with an empty-roster birth blob, and returns the
    /// blob's address; a second call reuses the custody key and creates nothing.
    #[test]
    fn provision_followers_tier_provisions_key_row_and_birth_blob_once() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());

        let material = block_on(a.provision_followers_tier()).expect("provision");

        {
            let reqs = nest.tiers_create_reqs.lock().unwrap();
            assert_eq!(reqs.len(), 1, "one reserved-name create");
            assert_eq!(reqs[0].name, FOLLOWERS_TIER);
            assert_eq!(
                i64::from(reqs[0].rank),
                FOLLOWERS_TIER_RANK,
                "the reserved rank-0 slot"
            );
            assert!(reqs[0].auto_approve, "follow = subscribe, no judgment");
            assert!(!reqs[0].hidden, "the followers tier is an ordinary offer");
            let upload = &reqs[0].encrypted_upload;
            assert!(
                upload_roster(upload).is_empty(),
                "a tier is born with an empty roster"
            );
        }

        // Custody holds the tier at version 1, and the returned material is it.
        let cfg = held_custody(&a);
        let period = custody::current_period(&cfg, FOLLOWERS_TIER).expect("period recorded");
        assert_eq!(period.version, 1);
        assert_eq!(*material.period_key, *period.key);
        assert_eq!(material.period_version, 1);
        assert_eq!(material.tier, FOLLOWERS_TIER);
        assert_eq!(i64::from(material.rank), FOLLOWERS_TIER_RANK);
        assert_eq!(
            material.key_blob_ref,
            nest.key_blobs.lock().unwrap()[FOLLOWERS_TIER].1
        );

        // Second call: the custody key is reused (a fresh one would orphan the
        // live blob) and the live blob means nothing is created.
        let again = block_on(a.provision_followers_tier()).expect("idempotent");
        assert_eq!(
            nest.tiers_create_reqs.lock().unwrap().len(),
            1,
            "a tier with a live blob is never re-created"
        );
        assert_eq!(*again.period_key, *material.period_key);
        assert_eq!(again.key_blob_ref, material.key_blob_ref);
    }

    /// The fourth combination — **custody empty while the nest already holds a
    /// followers `KeyBlob`** — is refused, not healed. Recording a fresh period
    /// key here would hand back material whose `period_key` and `key_blob_ref`
    /// name different keys, so every post gated with it would be unreadable by
    /// every follower and nothing downstream would notice; it would also strand
    /// every post already gated under the lost key. The machine skip-logs
    /// `Friends` content with this reason instead.
    #[test]
    fn provision_followers_tier_refuses_when_custody_lost_its_key_but_the_nest_has_a_blob() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        nest.set_key_blob(FOLLOWERS_TIER, 1, [0x5A; 32]);

        let Err(err) = block_on(a.provision_followers_tier()) else {
            panic!("material pairing a fresh key with someone else's blob is unreadable");
        };
        assert!(
            matches!(err, AuthorError::NoPeriodKey(ref t) if t == FOLLOWERS_TIER),
            "got {err:?}"
        );
        // Custody is left exactly as it was — a `record_new_tier` here would
        // strand every post already gated under the key that went missing.
        assert!(
            custody::current_period(&held_custody(&a), FOLLOWERS_TIER).is_none(),
            "the refusal must not write a period key"
        );
        assert!(
            nest.tiers_create_reqs.lock().unwrap().is_empty(),
            "the refusal mints nothing"
        );
    }

    /// A transport fault on the first `key_blob.get` is NOT "no blob": read as
    /// state (3) it would record a fresh key in custody beside whatever blob
    /// the nest actually holds. It propagates, nothing is written or minted,
    /// and once the fault clears the same call sees the nest honestly.
    #[test]
    fn provision_followers_tier_propagates_a_transport_fault_and_touches_nothing() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        nest.set_key_blob(FOLLOWERS_TIER, 1, [0x5A; 32]);
        nest.fail_key_blob_get(1);

        let Err(err) = block_on(a.provision_followers_tier()) else {
            panic!("a lost round trip must never be read as no blob");
        };
        assert!(matches!(err, AuthorError::Transport(_)), "got {err:?}");
        assert!(
            custody::current_period(&held_custody(&a), FOLLOWERS_TIER).is_none(),
            "a fault writes no period key"
        );
        assert!(nest.tiers_create_reqs.lock().unwrap().is_empty());

        // The fault cleared: the live blob is seen, custody is empty — the
        // lost-key refusal, never a fresh key beside someone else's blob.
        let Err(err) = block_on(a.provision_followers_tier()) else {
            panic!("custody empty beside a live blob is the lost-key state");
        };
        assert!(
            matches!(err, AuthorError::NoPeriodKey(ref t) if t == FOLLOWERS_TIER),
            "got {err:?}"
        );
        assert!(custody::current_period(&held_custody(&a), FOLLOWERS_TIER).is_none());
    }

    /// The nest keeps a blob it already holds and answers `created: false`
    /// with no other signal. If one lands between this client's read and its
    /// create — a sibling device winning the race — custody's key and the live
    /// blob name different keys: refused. Custody never deletes a key, so the
    /// period this call recorded stays; the next call is state (1), and it
    /// refuses the mismatched pair by the live blob's key witness instead of
    /// reusing it — until the sibling's key arrives and the two agree.
    #[test]
    fn provision_followers_tier_refuses_when_the_nest_kept_another_blob() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        nest.keep_foreign_blob_on_create([0x5B; 32]);

        let Err(err) = block_on(a.provision_followers_tier()) else {
            panic!("material pairing our key with a foreign blob is unreadable");
        };
        assert!(
            matches!(err, AuthorError::LiveBlobMismatch { ref tier } if tier == FOLLOWERS_TIER),
            "got {err:?}"
        );
        assert_eq!(
            nest.tiers_create_reqs.lock().unwrap().len(),
            1,
            "the create was attempted once"
        );
        let ours = custody::current_period(&held_custody(&a), FOLLOWERS_TIER)
            .expect("the period recorded for the attempt stays — custody deletes no key");

        // The sibling's blob, as the nest serves it: a real blob wrapping the
        // sibling's own key.
        let sibling = [0x5B; 32];
        nest.set_authored_key_blob(
            FOLLOWERS_TIER,
            &ActorKeypair::from_secret([0xA0; 32]),
            &[],
            5,
            sibling,
        );

        // Next call: custody holds a key, the nest a blob wrapping another —
        // state (1) reads the witness and refuses the pair, minting nothing.
        let Err(err) = block_on(a.provision_followers_tier()) else {
            panic!("material pairing our key with the sibling's blob is unreadable");
        };
        assert!(
            matches!(err, AuthorError::LiveBlobMismatch { ref tier } if tier == FOLLOWERS_TIER),
            "got {err:?}"
        );
        assert_eq!(nest.tiers_create_reqs.lock().unwrap().len(), 1);

        // The sibling's key arrives by walk and — the higher of two version-1
        // periods by the merge's order — becomes `current`: the pair agrees
        // and the material is handed out under the sibling's key.
        seed_custody(&a, |c| {
            custody::commit_period(
                c,
                FOLLOWERS_TIER,
                &TierPeriod {
                    version: 1,
                    key: sibling.into(),
                    rotated_at: ours.rotated_at + 1,
                    minted_by: Some(me()),
                },
            )
            .expect("the tier is held");
        });
        let material = block_on(a.provision_followers_tier()).expect("the pair agrees");
        assert_eq!(*material.period_key, sibling);
    }

    /// A nest that answers `tiers.list` with the owner-only row `hidden: false`
    /// (it stored the flag in `extra`) is refused — never gate to a tier the
    /// nest offers.
    #[test]
    fn provision_owner_only_tier_refuses_an_offered_reserved_row() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone());
        // `tier_item` builds an OFFERED row — exactly what a nest that
        // did not honour the `hidden` flag reports back.
        nest.set_tiers(&[tier_item(OWNER_ONLY_TIER, OWNER_ONLY_TIER_RANK, false)]);

        // `expect_err` would demand `Debug` on `TierGateMaterial`, whose
        // `period_key` is secret — the material deliberately has none.
        let Err(err) = block_on(a.provision_owner_only_tier()) else {
            panic!("an offered reserved row must never be gated to");
        };
        assert!(
            matches!(err, AuthorError::HiddenTierNotHonored),
            "got {err:?}"
        );
        assert!(
            nest.tiers_create_reqs.lock().unwrap().is_empty(),
            "the refusal mints nothing"
        );
    }
}
