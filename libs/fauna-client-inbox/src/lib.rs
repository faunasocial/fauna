//! Typed-call wrapper for the fauna-native inbox delivery-queue WS-RPC
//! kinds — the caller-scoped drain `fauna.inbox.fetch` + `fauna.inbox.ack`
//! (the per-actor store-and-forward queue: contact-requests, knocks,
//! group invites/messages, MLS Welcomes, security notices, cross-nest DMs)
//! plus the producing leg `fauna.inbox.send` (hand the home nest a signed
//! `(ContactRequest, Post)` tuple for same-nest local delivery or cross-nest
//! federation).
//!
//! Faithful transport migration of the HTTP inbox endpoints (tracked
//! internally): the `GET /api/v1/inbox/
//! {actor_id}` drain → fetch (peek) + ack (consume), whose split fixes the
//! HTTP twin's data-loss bug (it marked items delivered on *read*); and the
//! `POST /api/v1/inbox/{actor}` delivery → `send`. See `inbox.rs`.
//!
//! Pattern: same shape as the sibling per-feature client wrappers
//! (`fauna-client-search`, `-bridges`, `-conversations`) — a thin
//! `pub struct InboxClient { nest: R }`, one async method per kind, no state
//! machine. Generic over the WS-RPC transport (`R: RpcRequester`): native
//! call sites pass `Arc<NestClient>`, the wasm SPA passes its `WsRpcClient`.
//! The kind-composition logic is written once here and shared across native
//! + wasm (priority #2).
//!
//! **Drain loop the consumers run:** `fetch` (limit) → durably apply the
//! returned items → `ack` their ids → repeat while `reply.more`. The
//! `PushEvent::InboxItem` push is the prompt; this fetch+ack loop is the
//! delivery guarantee (push is best-effort).

use fauna_core::file_download::FileDownloadKeys;
use fauna_core::label_custody;
use fauna_core::path_crypto::SealedLabelRender;
use fauna_protocol::RpcRequester;
use fauna_protocol::inbox::{
    InboxAckReply, InboxAckRequest, InboxEnvelope, InboxFetchReply, InboxFetchRequest, InboxKind,
    InboxSendReply, InboxSendRequest, SecurityNoticeInbox, WelcomeInbox,
};

pub use fauna_protocol::inbox;

/// Typed `fauna.inbox.*` call surface. Caller-scoped by construction (no
/// `actor_id` param — the connection knows its caller). Errors propagate as
/// the transport's `R::Error` (native `NestClientError`, wasm rpc-wasm
/// error); the `fauna.inbox.permission_denied` `RpcError` the handler emits
/// surfaces through that error channel.
pub struct InboxClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> InboxClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.inbox.fetch` — peek the caller's undelivered inbox items
    /// without changing their status. Replay-safe read (`forbid_replay=false`,
    /// 5 s per `register_inbox_kinds`). `limit == 0` selects the handler
    /// default; page until `reply.more` is false, acking the applied ids
    /// between pages.
    pub async fn fetch(&self, limit: u32) -> Result<InboxFetchReply, R::Error> {
        self.fetch_after(limit, None).await
    }

    /// `fauna.inbox.fetch` with the skip cursor: return only undelivered items
    /// whose id is greater than `after_id` (`None` == from the oldest, i.e.
    /// exactly [`Self::fetch`]).
    ///
    /// Skipping is **not** acking — passed-over items stay undelivered. Only
    /// [`drain`] needs this; a display-only peek surface stays on `fetch` and
    /// never acks (`api-layers.md` § Inbox & Messaging, the ratified
    /// read-only-surface ack policy).
    pub async fn fetch_after(
        &self,
        limit: u32,
        after_id: Option<i64>,
    ) -> Result<InboxFetchReply, R::Error> {
        self.nest
            .request(
                "fauna.inbox.fetch",
                InboxFetchRequest {
                    limit,
                    after_id,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.inbox.ack` — mark the ids the caller has durably applied as
    /// delivered, so a re-fetch no longer returns them. Idempotent
    /// (already-delivered / not-owned ids are no-ops); the reply's `acked`
    /// counts the rows newly flipped.
    pub async fn ack(&self, ids: Vec<i64>) -> Result<InboxAckReply, R::Error> {
        self.nest
            .request(
                "fauna.inbox.ack",
                InboxAckRequest {
                    ids,
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.inbox.send` — the client→home-nest bearer leg of fauna-native
    /// social inbox delivery. Hands the caller's home nest the canonical
    /// signed `(ContactRequest, Post)` tuple (`payload_bytes`); the nest
    /// verifies the caller is the payload's sender, then **local-delivers**
    /// when `recipient_nest_url` is `None` (recipient on the caller's own home
    /// nest), or originates `fauna.federation.inbox.deliver` to that peer when
    /// `Some` (cross-nest). The reply's `inbox_id` is the created row id on
    /// delivery, or `None` when a knock was stored (`allow_knock` mode). This
    /// is the WS-RPC successor of the `POST /api/v1/inbox/{actor}` twin.
    pub async fn send(
        &self,
        recipient_actor_id: String,
        recipient_nest_url: Option<String>,
        payload_bytes: Vec<u8>,
    ) -> Result<InboxSendReply, R::Error> {
        self.nest
            .request(
                "fauna.inbox.send",
                InboxSendRequest {
                    recipient_actor_id,
                    recipient_nest_url,
                    payload_bytes,
                    extra: Default::default(),
                },
            )
            .await
    }
}

// ───────────────────────────── Shared drain-apply (layer 2) ─────────────────
//
// The durable inbox-apply consumer. Built **once** above the transport-thin
// `InboxClient` and consumed by every app — web over wasm, native over
// UniFFI (`api-layers.md` § Inbox & Messaging, layer 2; priority #2). The drain
// owns the fetch → decode-the-canonical-envelope → dispatch-by-`kind` → ack
// loop; each platform supplies an [`InboxApply`] that performs the actual,
// platform-specific apply (native wires `ingest_welcome` through the
// conversation engine, web through the wasm bridge — layers 3 & 4). This is the
// *only* surface that calls `fauna.inbox.ack` (the ratified ack policy:
// display-only badges peek-never-ack); `ack` means **durably applied**, so a
// crash before apply re-delivers rather than dropping.

/// Per-kind durable-apply hooks the shared [`drain`] dispatches to. Implement
/// once per client. An apply returns `Ok(())` **only** when the item was
/// durably applied — that is the signal the drain uses to `ack` it; a returned
/// error (or an undecodable / unknown-kind item) leaves the row un-acked so a
/// retry — or a newer client build — picks it up later. No item is ever dropped.
// Static-dispatch only (the generic `drain`), so we *want* per-impl `Send`
// inference (native `Send`, wasm `!Send`) — an explicit `Send` bound or
// `async_trait(?Send)` boxing would defeat it. Mirrors `RpcRequester`.
#[allow(async_fn_in_trait)]
pub trait InboxApply {
    /// Surfaced when a single apply fails. The drain treats it like a skip
    /// (leaves the item un-acked, counts it, continues the page) rather than
    /// aborting the whole drain on one bad item.
    type Error: core::fmt::Display;

    /// Apply an MLS Welcome: feed `welcome.welcome_bytes` to the conversation
    /// engine (`ingest_welcome`). For a cross-nest welcome `welcome.nest_url`
    /// addresses the next hop; `channel_id` / `channel_type` / `group_id` route
    /// it to the right surface (the metadata the best-effort push would carry).
    async fn apply_welcome(&self, welcome: WelcomeInbox) -> Result<(), Self::Error>;

    /// Apply a signed `(ContactRequest, Post)` tuple: `tuple_bytes` is the exact
    /// canonical pair the existing contact-request decode path consumes (the
    /// drain does not decode it — it rides through the envelope opaque).
    async fn apply_contact_request(&self, tuple_bytes: Vec<u8>) -> Result<(), Self::Error>;

    /// Surface a server-originated security notice (display-only; returning
    /// `Ok` acks it — there is nothing further to apply).
    async fn apply_security_notice(&self, notice: SecurityNoticeInbox) -> Result<(), Self::Error>;
}

/// What a single [`drain`] pass accomplished.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrainOutcome {
    /// Items durably applied and acked this pass.
    pub applied: usize,
    /// Items skipped — undecodable (old-format / corrupt), an unknown future
    /// `kind`, or an apply error. **Left un-acked** so a newer client build or a
    /// retry can still apply them; never dropped (no user-data loss).
    pub skipped: usize,
    /// `true` when undelivered items remain after this pass — either the nest
    /// still reports more past the last page, or the pass stepped over items it
    /// could not apply (`skipped > 0`), which stay undelivered by design.
    ///
    /// Note it is **not** "the drain stalled": with the skip cursor a pass walks
    /// the whole queue, so a `true` here alongside `applied > 0` is the ordinary
    /// steady state for an actor holding notices no app renders yet. The
    /// consumer re-invokes `drain` on its next tick regardless.
    pub more_pending: bool,
}

/// A transport error from the drain's own `fetch` / `ack` calls. Per-item apply
/// errors are **not** raised here — they are absorbed as skips (see
/// [`DrainOutcome::skipped`]) so one bad item can't abort the whole drain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainError<E> {
    /// `fauna.inbox.fetch` failed.
    Fetch(E),
    /// `fauna.inbox.ack` failed.
    Ack(E),
}

impl<E: core::fmt::Display> core::fmt::Display for DrainError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DrainError::Fetch(e) => write!(f, "inbox fetch failed: {e}"),
            DrainError::Ack(e) => write!(f, "inbox ack failed: {e}"),
        }
    }
}

/// Drain the caller's durable inbox once: `fetch` a page → decode each item's
/// canonical [`InboxEnvelope`] → dispatch by [`InboxKind`] to `apply` → `ack`
/// the ids durably applied → repeat **while** the nest reports more undelivered
/// items **and** the pass made progress (acked ≥ 1).
///
/// Paging is driven by a **monotonic skip cursor** (`after_id`), not by whether
/// the pass acked anything. That distinction is the whole point: an item the
/// client cannot apply — a kind whose surface does not exist yet
/// (`apply_security_notice` / `apply_contact_request`), an `InboxKind::Unknown`
/// from a newer nest, or an undecodable row — is left **un-acked but stepped
/// over**, so it no longer shadows everything behind it. Before the cursor, a
/// page filled entirely with such items ended the drain, and once they exceeded
/// one page the missed-push MLS-Welcome backstop stopped delivering silently and
/// permanently (`api-layers.md` § Inbox & Messaging, layer 3).
///
/// Skipping is **not** acking: passed-over items stay undelivered and come back
/// on the next drain, so nothing is dropped and nothing is consumed unrendered
/// (`critical-alerts.md` § Mechanism — acking a security notice no app renders
/// would destroy it). `more_pending` therefore reports that residue.
///
/// Termination has two guards: the cursor strictly increases each page, and a
/// nest that ignores `after_id` (and so re-returns the same head) is
/// detected by the page not advancing — the drain stops instead of spinning.
///
/// What to do after one page of a `fauna.inbox.fetch` walk.
///
/// Extracted so the inbox's **two** walkers — [`drain`] and
/// [`list_folder_pending_shares`] — cannot disagree about when a walk is
/// finished. They had disagreed in the worst possible way: the drain paged with
/// `after_id` and the pending-share peek did not page at all, so any residue the
/// drain deliberately steps past (unknown kinds a newer nest
/// introduced, undecodable rows, un-answered stranger knocks — all un-acked *by
/// design*) silently pushed real shares off the peek's single page, making them
/// impossible to accept **or** decline with no error anywhere.
fn inbox_page_step(after_id: Option<i64>, page_max: Option<i64>, more: bool) -> InboxPageStep {
    // A nest that ignores `after_id` hands back the same head (hostile or
    // buggy). Detect the non-advance rather than looping on it forever.
    if let (Some(sent), Some(max)) = (after_id, page_max)
        && max <= sent
    {
        return InboxPageStep::StalledOnSkew;
    }
    match page_max {
        Some(max) if more => InboxPageStep::Advance(max),
        _ => InboxPageStep::Done,
    }
}

/// The outcome of [`inbox_page_step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InboxPageStep {
    /// The nest ignored the cursor (version skew) — stop; residue remains.
    StalledOnSkew,
    /// Fetch the next page with the cursor set to this id.
    Advance(i64),
    /// The queue is walked.
    Done,
}

/// `page_limit == 0` selects the handler default. Returns on the first `fetch`
/// or `ack` transport error; per-item apply errors are absorbed as skips.
pub async fn drain<R, A>(
    client: &InboxClient<R>,
    apply: &A,
    page_limit: u32,
) -> Result<DrainOutcome, DrainError<R::Error>>
where
    R: RpcRequester,
    A: InboxApply,
{
    let mut outcome = DrainOutcome::default();
    let mut after_id: Option<i64> = None;
    loop {
        let reply = client
            .fetch_after(page_limit, after_id)
            .await
            .map_err(DrainError::Fetch)?;
        let page_max = reply.items.iter().map(|i| i.id).max();
        let step = inbox_page_step(after_id, page_max, reply.more);

        // Termination guard: a nest that ignores
        // `after_id` hands back the same head. Detect the non-advance and
        // stop — the un-ackable residue waits for a nest that can step past it.
        // Checked BEFORE applying, so a repeated head is never applied twice.
        if matches!(step, InboxPageStep::StalledOnSkew) {
            outcome.more_pending = true;
            return Ok(outcome);
        }

        let mut applied_ids = Vec::new();
        for item in reply.items {
            let id = item.id;
            match apply_one(apply, item.payload).await {
                Ok(true) => applied_ids.push(id),
                // Decode/unknown skip, or an apply error — leave un-acked, and
                // step past it rather than letting it block the tail.
                Ok(false) | Err(_) => outcome.skipped += 1,
            }
        }
        if !applied_ids.is_empty() {
            outcome.applied += applied_ids.len();
            client.ack(applied_ids).await.map_err(DrainError::Ack)?;
        }

        match step {
            // Advance past this page. Un-acked items behind the cursor stay
            // undelivered; a later cursor-less drain re-offers them.
            InboxPageStep::Advance(max) => after_id = Some(max),
            // Walked the queue: `more_pending` is the un-acked residue, if any.
            InboxPageStep::Done => {
                outcome.more_pending = reply.more || outcome.skipped > 0;
                return Ok(outcome);
            }
            // Returned above, before anything was applied.
            InboxPageStep::StalledOnSkew => unreachable!("handled before apply"),
        }
    }
}

/// Decode + dispatch one inbox item. `Ok(true)` = durably applied (ack it);
/// `Ok(false)` = skip and leave un-acked (undecodable / unknown kind);
/// `Err` = the apply itself failed (skip, retry later).
async fn apply_one<A: InboxApply>(apply: &A, payload: Vec<u8>) -> Result<bool, A::Error> {
    let env = match InboxEnvelope::from_canonical_bytes(&payload) {
        Ok(e) => e,
        // Old-format / corrupt row — never crash the drain, never ack (so it is
        // not dropped). Negligible in practice (the queue was dormant).
        Err(_) => return Ok(false),
    };
    match env.kind {
        InboxKind::Welcome => match env.decode_welcome() {
            Ok(w) => {
                apply.apply_welcome(w).await?;
                Ok(true)
            }
            Err(_) => Ok(false),
        },
        InboxKind::ContactRequest => {
            apply.apply_contact_request(env.payload).await?;
            Ok(true)
        }
        InboxKind::SecurityNotice => match env.decode_security_notice() {
            Ok(n) => {
                apply.apply_security_notice(n).await?;
                Ok(true)
            }
            Err(_) => Ok(false),
        },
        // A room invitation is a **knock**: it is retained un-acked and never
        // applied here, because there is nothing to apply — accepting is the
        // user's decision, and it is the accept (or a decline) that consumes
        // the row. So there is no per-client `InboxApply` hook for it: the
        // retention is written once and every app inherits it identically
        // (priorities #1/#2), and [`list_pending_room_invites`] is the surface
        // that reads it back. Deliberately the same *mechanism* as the generic
        // unknown-kind retention: the invite stays un-acked until an accept or a
        // decline consumes it.
        InboxKind::RoomInvite => Ok(false),
        // Forward-compat: a kind a newer nest introduced. Skip, leave un-acked
        // for a client build that understands it (additive-everywhere).
        InboxKind::Unknown => Ok(false),
    }
}

// ── Recipient folder pending-share list (peek, never ack) ─────────────────

/// A staged, not-yet-accepted cross-user folder share — one un-acked
/// `channel_type == "folder"` [`WelcomeInbox`] the recipient contact gate left
/// as a **knock** (a stranger's / Blocked / unstamped share). A *contact's*
/// share is auto-joined + acked by the gate, so it never appears here. This is
/// the row a client renders as a `folder-pending-share`, then **accepts** (join
/// the group off the chat rail, then [`InboxClient::ack`] the `inbox_id`) or
/// **declines** (a bare [`InboxClient::ack`], never joins).
/// `docs/goal/ui/folders.md` § Sharing a folder (Recipient side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderPendingShare {
    /// The durable-inbox row id — the accept/decline `ack` target.
    pub inbox_id: i64,
    /// The nest-stamped sharer (hex ActorId), when present. `None` for an
    /// unstamped / cross-nest-relayed share — which the gate knocked *because*
    /// it could not name the sharer to weigh contact-status.
    pub shared_by: Option<String>,
    /// The sharer's handle — bare for a same-nest sharer (nest-resolved from
    /// [`Self::shared_by`]), paired with [`Self::shared_by_domain`] for a
    /// cross-nest sharer whose domain the recipient's own nest verified
    /// (`federation.md` § Cross-nest shared folders + channel append → *The
    /// cross-nest owner label*). `None` for a handle-less or unverified sharer;
    /// render [`Self::shared_by_display`], which already folds in the join and
    /// the fallback.
    pub shared_by_handle: Option<String>,
    /// The cross-nest sharer's handle domain — `None` same-nest (a bare handle
    /// means a local user). Joined with [`Self::shared_by_handle`] at display.
    pub shared_by_domain: Option<String>,
    /// The pre-computed "Shared by ‹…›" label — the one string a client renders,
    /// so the six apps cannot drift on the fallback truncation (the
    /// per-app 12-char/ellipsis variants this replaces; priorities #1/#4).
    /// Delegates to `fauna_core::format::account_display_label` over
    /// `fauna_core::format::qualified_handle`: the canonical `handle@domain`
    /// for a verified cross-nest sharer, the bare handle for a local one, else
    /// the canonical `short_id` of the sharer hex. Empty string only for a
    /// fully unstamped cross-nest share (`shared_by` AND `shared_by_handle`
    /// both absent — a pair that did not verify) — the client renders its own
    /// unknown-sharer i18n label for that case, the one genuinely
    /// locale-dependent branch.
    pub shared_by_display: String,
    /// The MLS app-level group id (hex), when the Welcome carried one.
    pub group_id: Option<String>,
    /// The fauna channel id (hex) — the accept path's `join_folder_welcome`
    /// channel address.
    pub channel_id: Option<String>,
    /// The share's home-nest URL (the staged `WelcomeInbox.nest_url`) — threaded
    /// into `join_folder_welcome` on accept so a cross-nest share's later
    /// content fetch relays to the right nest. `None` for a same-nest share.
    pub home_nest_url: Option<String>,
    /// The shared set's name, nest-resolved on the set's HOME nest from its
    /// claimed row (the staged `WelcomeInbox.set_name`, sealed-first rendered
    /// through [`label_custody::render_set_name`] — path-sealing S5c-2) —
    /// names the pending-share row and, on accept, the recipient's foreign-set
    /// `fauna.state.folder-keys` row (a cross-nest recipient's own nest holds no row to
    /// resolve a name from). `None` when the seal
    /// cannot be opened — which, once the plaintext scrubs at the flip, is
    /// EVERY pending share: a share not yet accepted is by definition a group
    /// this reader hasn't joined, so it never holds the MLS content key that
    /// opens the seal (clients fall back to their unknown-set i18n label
    /// either way). Display-only — never a lookup key.
    pub set_name: Option<String>,
    /// The recipient's access grant on the set (`"reader"`/`"writer"`), resolved
    /// on the set's HOME nest from its `folder_member_access` row (the staged
    /// `WelcomeInbox.access`) — threaded into `join_folder_welcome` on accept
    /// so a cross-nest member's foreign-set row knows whether this client may
    /// offer a folder binding. `None` (no role row) ⇒ unknown ⇒ reader.
    /// **Advisory-for-UI only — never an authorization input**
    /// (`docs/goal/architecture/federation.md` § Cross-nest → *Recipient-side
    /// access discovery*).
    pub access: Option<String>,
    /// The home nest's deployment `nest_actor_id` (the staged
    /// `WelcomeInbox.home_nest_actor_id`) — threaded into `join_folder_welcome`
    /// on accept as the byte-plane SPKI-pin trust root for a cross-nest share.
    /// `None` same-nest / relay-unaware ⇒ the byte plane keeps `RequireWebPki`.
    pub home_nest_actor_id: Option<String>,
    /// The raw MLS Welcome bytes — fed to `join_folder_welcome` on accept.
    pub welcome_bytes: Vec<u8>,
    /// The set name sealed under the set's content keys (the staged
    /// `WelcomeInbox.set_name_sealed` + `set_name_hash`) — unopenable here, so
    /// [`Self::set_name`] is `None`, but threaded into `join_folder_welcome`
    /// on accept, which opens it once the join has ingested those keys and
    /// names the recipient's foreign-set record with it.
    pub set_name_seal: Option<label_custody::SealedSetName>,
}

/// Page size for the pending-share walk: the nest's `fauna.inbox.fetch` cap
/// (`INBOX_FETCH_MAX_LIMIT`).
///
/// ⚠ This used to say *"a peek is cursor-less (a second peek re-returns the same
/// head), so one page is all `list_folder_pending_shares` can see"*. **That was
/// false, and the belief was the bug**: `InboxFetchRequest` has
/// always carried `after_id` and the reply `more`, [`drain`] has always paged
/// with them, and the peek simply never inherited it. The premise that "knocks
/// are few in practice" then licensed a single page — but the queue ahead of a
/// knock is not knocks, it is the un-ackable residue the compat rule
/// *guarantees* accumulates (unknown kinds from a newer nest, undecodable rows),
/// and 500 of those made every later share invisible and un-declinable.
/// [`list_folder_pending_shares`] now walks with the cursor; this is the size of
/// each step, not the reach of the whole walk.
///
/// Owned here, beside the list it parameterizes, so the five call sites (fauna-ffi,
/// fauna-wasm, linux, and the shared accept/decline recipes) cannot drift on the
/// page size — they used to hold three hand-synced copies cross-referencing each
/// other by comment (priorities #1/#4).
pub const PENDING_SHARE_PEEK_LIMIT: u32 = 500;

/// How many pages [`list_folder_pending_shares`] will walk before giving up —
/// the stated bound that keeps a pathological queue from becoming an unbounded
/// client loop on a surface the user is actively waiting on.
///
/// At the default [`PENDING_SHARE_PEEK_LIMIT`] page size this examines 10,000
/// inbox rows, which is orders of magnitude above any real inbox: reaching it
/// means the residue itself is the problem. The degradation is deliberately
/// *some shares not listed* rather than a hang — the same posture the
/// version-skew guard takes.
pub const PENDING_SHARE_MAX_PAGES: u32 = 20;

/// Peek the caller's durable inbox and return every staged folder share (an
/// un-acked `channel_type == "folder"` Welcome) — the pending-share list the
/// recipient's `folder-pending-share` area renders. A **peek**
/// ([`InboxClient::fetch`], no `ack`), so listing never consumes a knock.
///
/// After the normal receive drain runs, the only un-acked folder welcomes are
/// the knocks: the contact gate already acked the `Auto` (joined) and `Suppress`
/// (dropped) dispositions, leaving just the stranger / Blocked / unstamped knocks
/// here. Written once, shared native + wasm (priority #2). `page_limit == 0`
/// selects the handler default.
///
/// **Walks the queue with the skip cursor**, sharing
/// [`drain`]'s termination logic through [`inbox_page_step`] — peeking, never
/// acking, so a listing still consumes no knock. It must page: the rows ahead of
/// a share are the residue the additive-everywhere rule guarantees
/// (`version-compatibility.md`) — a client older than its nest accumulates
/// unknown-kind rows *permanently* — and one page of those hid every share
/// behind them, with accept and decline both impossible and no error rendered.
///
/// **Bounded at [`PENDING_SHARE_MAX_PAGES`] pages** (so at most
/// `PENDING_SHARE_MAX_PAGES × page_limit` rows are examined): a pathological
/// queue degrades to *some shares not listed* rather than an unbounded client
/// loop on a surface a user is waiting on. The bound is deliberately far above
/// any real inbox; hitting it means something is wrong upstream, not that the
/// bound is too small.
pub async fn list_folder_pending_shares<R: RpcRequester>(
    client: &InboxClient<R>,
    page_limit: u32,
) -> Result<Vec<FolderPendingShare>, R::Error> {
    let mut shares = Vec::new();
    let mut after_id: Option<i64> = None;
    for _ in 0..PENDING_SHARE_MAX_PAGES {
        let reply = client.fetch_after(page_limit, after_id).await?;
        let page_max = reply.items.iter().map(|i| i.id).max();
        let step = inbox_page_step(after_id, page_max, reply.more);
        collect_folder_shares(reply.items, &mut shares);
        match step {
            InboxPageStep::Advance(max) => after_id = Some(max),
            // Walked the queue, or a nest that ignores the cursor and cannot step past its own
            // residue — either way this is everything a peek can honestly show.
            InboxPageStep::Done | InboxPageStep::StalledOnSkew => break,
        }
    }
    Ok(shares)
}

/// Fold one fetched page's items into the pending-share list.
///
/// Split out of [`list_folder_pending_shares`] so the per-item filtering is
/// written once across the walk's pages rather than once per loop body.
fn collect_folder_shares(
    items: Vec<fauna_protocol::inbox::InboxItem>,
    shares: &mut Vec<FolderPendingShare>,
) {
    for item in items {
        // Undecodable / non-Welcome / undecodable-welcome rows are skipped (never
        // ack'd — this is a read); mirrors `apply_one`'s tolerance.
        let Ok(env) = InboxEnvelope::from_canonical_bytes(&item.payload) else {
            continue;
        };
        if env.kind != InboxKind::Welcome {
            continue;
        }
        let Ok(welcome) = env.decode_welcome() else {
            continue;
        };
        // The `channel_type` wire tag the nest stamps for a cross-user shared
        // folder Welcome (matches `WelcomeKind::Folder`'s snake_case tag +
        // `wire_channel_type_to_kind`'s `"folder"` arm).
        if welcome.channel_type.as_deref() != Some("folder") {
            continue;
        }
        let shared_by_display = fauna_core::format::account_display_label(
            fauna_core::format::qualified_handle(
                welcome.shared_by_handle.as_deref(),
                welcome.shared_by_domain.as_deref(),
            )
            .as_deref(),
            welcome.shared_by.as_deref().unwrap_or(""),
        );
        // The set-name pair, through the shared render seam (path-sealing
        // S5c-2). Always rendered keyless: a *pending* share is by
        // definition a group this reader has not yet joined, so it never
        // holds MLS content keys for it — there is no custody to thread in.
        // `Omit` nulls `set_name` (the client's existing unknown-set i18n
        // fallback) rather than dropping the row; a share the user must
        // accept/decline cannot vanish from the list.
        let set_name = match label_custody::render_set_name(
            &FileDownloadKeys::default(),
            welcome.set_name_sealed.as_deref().map(|b| &b[..]),
            welcome.set_name.as_deref().unwrap_or(""),
            welcome.set_name_hash.as_deref().map(|b| &b[..]),
        ) {
            SealedLabelRender::Sealed(name) => Some(name),
            SealedLabelRender::Plaintext(name) => Some(name),
            SealedLabelRender::Omit => None,
        };
        shares.push(FolderPendingShare {
            inbox_id: item.id,
            shared_by: welcome.shared_by,
            shared_by_handle: welcome.shared_by_handle,
            shared_by_domain: welcome.shared_by_domain,
            shared_by_display,
            group_id: welcome.group_id,
            channel_id: welcome.channel_id,
            home_nest_url: welcome.nest_url,
            set_name,
            access: welcome.access,
            home_nest_actor_id: welcome.home_nest_actor_id,
            welcome_bytes: welcome.welcome_bytes,
            set_name_seal: label_custody::SealedSetName::from_wire(
                welcome.set_name_sealed.as_deref().map(|b| &b[..]),
                welcome.set_name_hash.as_deref().map(|b| &b[..]),
            ),
        });
    }
}

// ── Pending room invitations (peek, never ack) ────────────────────────────

/// One un-acked room invitation staged in the invitee's durable inbox — the
/// knock a room's `invite` raised (`conversation-rooms.md` § Join rules and
/// invites). The reader **verifies** `signed_invite` before naming anybody: this
/// layer holds no MLS vocabulary, so the bytes ride through opaque exactly as
/// they arrived, and the room-plane consumer decodes and checks them.
///
/// Consumed by accepting (accept the invitation, then [`InboxClient::ack`] the
/// `inbox_id`) or by declining (a bare [`InboxClient::ack`]) — the shape a
/// staged folder share already has ([`FolderPendingShare`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRoomInvite {
    /// The durable-inbox row id — the accept/decline `ack` target.
    pub inbox_id: i64,
    /// Canonical DAG-CBOR `SignedRoomInvite` bytes, verbatim as signed.
    pub signed_invite: Vec<u8>,
    /// The room's home nest when it is not the invitee's own — the address
    /// the invitee's nest bound from the delivering connection's verified
    /// identity, and what the relayed accept is aimed at; `None` same-nest.
    pub room_node: Option<String>,
}

/// Peek the caller's durable inbox and return every staged room invitation — a
/// **peek** ([`InboxClient::fetch`], no `ack`), so listing never consumes a
/// knock.
///
/// Every [`InboxKind::RoomInvite`] row is un-acked by construction: [`drain`]
/// retains the kind rather than applying it, because accepting is a decision
/// only the user makes. So this walk sees the whole standing set of pending
/// invitations, not a residue of failed applies.
///
/// Walks with the skip cursor and is bounded exactly as
/// [`list_folder_pending_shares`] is, and for the same measured reason: the rows
/// ahead of an invitation are the un-ackable residue the additive-everywhere
/// rule guarantees accumulates, and one page of those would hide every
/// invitation behind them with no error rendered. `page_limit == 0` selects the
/// handler default.
pub async fn list_pending_room_invites<R: RpcRequester>(
    client: &InboxClient<R>,
    page_limit: u32,
) -> Result<Vec<PendingRoomInvite>, R::Error> {
    let mut invites = Vec::new();
    let mut after_id: Option<i64> = None;
    for _ in 0..PENDING_SHARE_MAX_PAGES {
        let reply = client.fetch_after(page_limit, after_id).await?;
        let page_max = reply.items.iter().map(|i| i.id).max();
        let step = inbox_page_step(after_id, page_max, reply.more);
        collect_room_invites(reply.items, &mut invites);
        match step {
            InboxPageStep::Advance(max) => after_id = Some(max),
            // Walked the queue, or a nest that ignores the cursor and cannot step past its own
            // residue — either way this is everything a peek can honestly show.
            InboxPageStep::Done | InboxPageStep::StalledOnSkew => break,
        }
    }
    Ok(invites)
}

/// Fold one fetched page's items into the pending-invitation list. Split out of
/// [`list_pending_room_invites`] for [`collect_folder_shares`]'s reason.
fn collect_room_invites(
    items: Vec<fauna_protocol::inbox::InboxItem>,
    invites: &mut Vec<PendingRoomInvite>,
) {
    for item in items {
        // Undecodable / other-kind / undecodable-payload rows are skipped (never
        // ack'd — this is a read); mirrors `apply_one`'s tolerance.
        let Ok(env) = InboxEnvelope::from_canonical_bytes(&item.payload) else {
            continue;
        };
        if env.kind != InboxKind::RoomInvite {
            continue;
        }
        let Ok(invite) = env.decode_room_invite() else {
            continue;
        };
        invites.push(PendingRoomInvite {
            inbox_id: item.id,
            signed_invite: invite.signed_invite,
            room_node: invite.room_node,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = InboxClient::new(MockRequester);
    }

    // ── Wire-contract tests ─────────────────────────────────────────────────
    //
    // Pin the exact kind strings + that the payloads round-trip to the typed
    // requests, so a kind rename here can't silently break the adapter. Real
    // end-to-end round-trip conformance lives in
    // `bins/fauna-nest/tests/conformance_inbox.rs` (real router dispatch).

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.inbox.fetch" => fauna_protocol::encode_canonical(&InboxFetchReply {
                extra: Default::default(),
                items: vec![],
                more: false,
            }),
            "fauna.inbox.ack" => fauna_protocol::encode_canonical(&InboxAckReply {
                extra: Default::default(),
                acked: 0,
            }),
            "fauna.inbox.send" => fauna_protocol::encode_canonical(&InboxSendReply {
                extra: Default::default(),
                inbox_id: Some(7),
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn fetch_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = InboxClient::new(rec.clone());
        block_on(client.fetch(50)).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.inbox.fetch");
        let req: InboxFetchRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.limit, 50);
    }

    #[test]
    fn ack_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = InboxClient::new(rec.clone());
        block_on(client.ack(vec![1, 7, 42])).expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.inbox.ack");
        let req: InboxAckRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.ids, vec![1, 7, 42]);
    }

    #[test]
    fn send_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = InboxClient::new(rec.clone());
        let reply = block_on(client.send("ab".repeat(32), None, vec![0xca, 0xfe, 0xba, 0xbe]))
            .expect("infallible mock");
        assert_eq!(reply.inbox_id, Some(7));

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.inbox.send");
        let req: InboxSendRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.recipient_actor_id, "ab".repeat(32));
        assert!(req.recipient_nest_url.is_none());
        assert_eq!(req.payload_bytes, vec![0xca, 0xfe, 0xba, 0xbe]);
    }

    #[test]
    fn send_carries_cross_nest_peer_url() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = InboxClient::new(rec.clone());
        block_on(client.send(
            "cd".repeat(32),
            Some("https://peer.example".into()),
            vec![1, 2, 3],
        ))
        .expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.inbox.send");
        let req: InboxSendRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(
            req.recipient_nest_url.as_deref(),
            Some("https://peer.example")
        );
    }

    // ── Shared drain-apply (layer 2) ────────────────────────────────────────

    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Scripted `fauna.inbox.fetch` pages + recorded `fauna.inbox.ack` ids. The
    /// last scripted page **repeats** when the queue is down to one entry, so a
    /// single-page `more: true` script models the nest re-returning the same
    /// undelivered head — which would hang the test if the drain's progress
    /// guard were missing.
    #[derive(Default)]
    struct DrainMock {
        fetch_pages: Mutex<VecDeque<InboxFetchReply>>,
        acked: Mutex<Vec<Vec<i64>>>,
        fetch_count: Mutex<usize>,
    }

    impl DrainMock {
        fn new(pages: Vec<InboxFetchReply>) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                fetch_pages: Mutex::new(pages.into()),
                ..Default::default()
            })
        }
    }

    impl RpcRequester for DrainMock {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let reply_bytes = match kind {
                "fauna.inbox.fetch" => {
                    *self.fetch_count.lock().unwrap() += 1;
                    let mut q = self.fetch_pages.lock().unwrap();
                    let page = if q.len() > 1 {
                        q.pop_front().unwrap()
                    } else {
                        q.front().cloned().unwrap_or_default()
                    };
                    fauna_protocol::encode_canonical(&page).unwrap()
                }
                "fauna.inbox.ack" => {
                    let bytes = fauna_protocol::encode_canonical(&payload).unwrap();
                    let req: InboxAckRequest = fauna_protocol::decode_strict(&bytes).unwrap();
                    let acked = req.ids.len() as u32;
                    self.acked.lock().unwrap().push(req.ids);
                    fauna_protocol::encode_canonical(&InboxAckReply {
                        extra: Default::default(),
                        acked,
                    })
                    .unwrap()
                }
                other => panic!("DrainMock: unexpected kind {other}"),
            };
            Ok(fauna_protocol::decode_strict(&reply_bytes).unwrap())
        }
    }

    /// Records which kind each item dispatched to, in order.
    #[derive(Default)]
    struct RecordingApply {
        applied: Mutex<Vec<String>>,
    }

    impl InboxApply for RecordingApply {
        type Error = std::convert::Infallible;

        async fn apply_welcome(&self, w: WelcomeInbox) -> Result<(), Self::Error> {
            self.applied
                .lock()
                .unwrap()
                .push(format!("welcome:{}", w.channel_id.unwrap_or_default()));
            Ok(())
        }

        async fn apply_contact_request(&self, tuple: Vec<u8>) -> Result<(), Self::Error> {
            self.applied
                .lock()
                .unwrap()
                .push(format!("contact_request:{:?}", tuple));
            Ok(())
        }

        async fn apply_security_notice(&self, n: SecurityNoticeInbox) -> Result<(), Self::Error> {
            self.applied
                .lock()
                .unwrap()
                .push(format!("security_notice:{}", n.subject));
            Ok(())
        }
    }

    fn item(id: i64, payload: Vec<u8>) -> fauna_protocol::inbox::InboxItem {
        fauna_protocol::inbox::InboxItem {
            extra: Default::default(),
            id,
            payload,
        }
    }

    fn welcome_env(channel: &str) -> Vec<u8> {
        InboxEnvelope::welcome(&WelcomeInbox {
            welcome_bytes: vec![1, 2, 3],
            channel_id: Some(channel.into()),
            channel_type: Some("dm".into()),
            ..Default::default()
        })
        .unwrap()
        .to_canonical_bytes()
        .unwrap()
    }

    fn unknown_env() -> Vec<u8> {
        // A `kind` a newer nest introduced — mirror the envelope map shape with a
        // String discriminator + a byte-string payload so it decodes to
        // `InboxKind::Unknown` (not as an undecodable row).
        #[derive(serde::Serialize)]
        struct RawEnvelope {
            kind: String,
            #[serde(with = "serde_bytes")]
            payload: Vec<u8>,
        }
        fauna_protocol::encode_canonical(&RawEnvelope {
            kind: "cross_nest_dm".into(),
            payload: vec![0x09, 0x09],
        })
        .unwrap()
        .to_vec()
    }

    /// A cross-user shared folder Welcome envelope (`channel_type == "folder"`),
    /// carrying the group id + the nest-stamped `shared_by` + resolved
    /// `shared_by_handle` (the knock the gate leaves un-acked).
    fn folder_welcome_env(
        channel: &str,
        group_id: &str,
        shared_by: Option<&str>,
        shared_by_handle: Option<&str>,
    ) -> Vec<u8> {
        InboxEnvelope::welcome(&WelcomeInbox {
            welcome_bytes: vec![9, 8, 7],
            channel_id: Some(channel.into()),
            channel_type: Some("folder".into()),
            group_id: Some(group_id.into()),
            shared_by: shared_by.map(Into::into),
            shared_by_handle: shared_by_handle.map(Into::into),
            // The home-nest-resolved grant every folder welcome now carries
            // (Phase 4 discovery seed) — pinned here so the DTO mapping below
            // can assert it survives the envelope decode.
            access: Some("writer".into()),
            // The home nest's deployment identity (byte-plane pin trust root) +
            // owner cadence, carried the same way — assert they reach the DTO.
            home_nest_actor_id: Some("ab".repeat(32)),
            // The scrubbed set name, carried sealed — unopenable before the
            // join, so it must reach the DTO for the accept to open it.
            set_name_sealed: Some(vec![0xED; 40].into()),
            set_name_hash: Some(vec![0x5A; 32].into()),
            ..Default::default()
        })
        .unwrap()
        .to_canonical_bytes()
        .unwrap()
    }

    /// A `fauna.inbox.fetch` mock that HONOURS `after_id`, the way
    /// `bins/fauna-nest/src/inbox_handlers.rs` does: undelivered rows oldest
    /// first, `limit` at a time, `more` set when any remain behind the page.
    ///
    /// `DrainMock` above cannot serve this test — it pops scripted pages in
    /// order and ignores the cursor, so a walker that never advanced would still
    /// be handed page 2 and the test would pass on the broken code. Modelling
    /// the cursor is the whole point.
    struct PagingInboxMock {
        items: Vec<fauna_protocol::inbox::InboxItem>,
        acked: Mutex<Vec<i64>>,
    }

    impl RpcRequester for PagingInboxMock {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).unwrap();
            let reply_bytes = match kind {
                "fauna.inbox.fetch" => {
                    let req: InboxFetchRequest = fauna_protocol::decode_strict(&bytes).unwrap();
                    let after = req.after_id.unwrap_or(i64::MIN);
                    let remaining: Vec<_> = self
                        .items
                        .iter()
                        .filter(|i| i.id > after)
                        .cloned()
                        .collect();
                    let limit = req.limit.max(1) as usize;
                    let page: Vec<_> = remaining.iter().take(limit).cloned().collect();
                    fauna_protocol::encode_canonical(&InboxFetchReply {
                        extra: Default::default(),
                        more: remaining.len() > page.len(),
                        items: page,
                    })
                    .unwrap()
                }
                "fauna.inbox.ack" => {
                    let req: InboxAckRequest = fauna_protocol::decode_strict(&bytes).unwrap();
                    self.acked.lock().unwrap().extend(req.ids.iter().copied());
                    fauna_protocol::encode_canonical(&InboxAckReply {
                        extra: Default::default(),
                        acked: 0,
                    })
                    .unwrap()
                }
                other => panic!("PagingInboxMock: unexpected kind {other}"),
            };
            Ok(fauna_protocol::decode_strict(&reply_bytes).unwrap())
        }
    }

    /// **a share sitting behind a full page of un-ackable residue is
    /// still listed.**
    ///
    /// The residue is not contrived: `apply_one` deliberately never acks an
    /// unknown-kind row (*"leave un-acked for a client build that understands
    /// it"*), so a client older than its nest accumulates them permanently —
    /// the additive-everywhere rule makes that an expected steady state, not an
    /// edge case. Before the fix the peek fetched exactly one cursor-less page,
    /// so the share below was invisible in every app and could be neither
    /// accepted nor declined — the pending list is the only surface that names
    /// an `inbox_id` — and nothing rendered an error.
    ///
    /// RED-VERIFIED against the pre-fix body (`client.fetch(page_limit)`): the
    /// share is absent and this fails on `shares.len()`.
    #[test]
    fn list_pending_shares_steps_past_a_full_page_of_unackable_residue() {
        let mut items: Vec<_> = (1..=i64::from(PENDING_SHARE_PEEK_LIMIT) + 1)
            .map(|id| item(id, unknown_env()))
            .collect();
        let share_id = i64::from(PENDING_SHARE_PEEK_LIMIT) + 2;
        items.push(item(
            share_id,
            folder_welcome_env("fschan", "abcd", Some("beef"), Some("carol")),
        ));

        let mock = std::sync::Arc::new(PagingInboxMock {
            items,
            acked: Mutex::new(Vec::new()),
        });
        let client = InboxClient::new(mock.clone());
        let shares = block_on(list_folder_pending_shares(
            &client,
            PENDING_SHARE_PEEK_LIMIT,
        ))
        .expect("infallible mock");

        assert_eq!(
            shares.len(),
            1,
            "a folder share behind {} un-ackable rows must still be listed: it is \
             the only surface that names an inbox_id, so an unlisted share can be \
             neither accepted nor declined",
            PENDING_SHARE_PEEK_LIMIT + 1
        );
        assert_eq!(shares[0].inbox_id, share_id);
        assert!(
            mock.acked.lock().unwrap().is_empty(),
            "listing is a PEEK: walking the queue must never ack, or the walk \
             would consume the very knocks it is listing"
        );
    }

    /// The walk stops rather than looping forever against a nest that ignores
    /// `after_id` — the version-skew guard, shared with `drain` through
    /// `inbox_page_step`. Without it the client hangs on such a nest, on a
    /// surface the user is actively waiting on.
    #[test]
    fn list_pending_shares_stops_on_a_nest_that_ignores_the_cursor() {
        struct SkewMock;
        impl RpcRequester for SkewMock {
            type Error = std::convert::Infallible;
            async fn request<Req, Reply>(
                &self,
                kind: &'static str,
                _payload: Req,
            ) -> Result<Reply, Self::Error>
            where
                Req: serde::Serialize,
                Reply: serde::de::DeserializeOwned,
            {
                assert_eq!(kind, "fauna.inbox.fetch");
                let bytes = fauna_protocol::encode_canonical(&InboxFetchReply {
                    extra: Default::default(),
                    items: vec![item(1, unknown_env())],
                    more: true,
                })
                .unwrap();
                Ok(fauna_protocol::decode_strict(&bytes).unwrap())
            }
        }

        let client = InboxClient::new(std::sync::Arc::new(SkewMock));
        let shares = block_on(list_folder_pending_shares(&client, 10)).expect("terminates");
        assert!(shares.is_empty());
    }

    #[test]
    fn list_pending_shares_returns_only_folder_welcomes_without_acking() {
        let mock = DrainMock::new(vec![InboxFetchReply {
            extra: Default::default(),
            items: vec![
                item(1, welcome_env("dmchan")), // a DM welcome — excluded
                item(
                    7,
                    folder_welcome_env("fschan", "abcd", Some("beef"), Some("carol")),
                ), // the knock
                item(3, unknown_env()),         // forward-compat — excluded
                item(4, vec![0x00]),            // undecodable — excluded
            ],
            more: false,
        }]);
        let client = InboxClient::new(mock.clone());

        let shares =
            block_on(list_folder_pending_shares(&client, 0)).expect("infallible transport");

        assert_eq!(
            shares,
            vec![FolderPendingShare {
                inbox_id: 7,
                shared_by: Some("beef".into()),
                shared_by_handle: Some("carol".into()),
                shared_by_domain: None,
                shared_by_display: "carol".into(),
                group_id: Some("abcd".into()),
                channel_id: Some("fschan".into()),
                home_nest_url: None,
                set_name: None,
                // The staged grant reaches the pending-share DTO, which is what
                // `folders_accept_share` threads into the accept-time
                // foreign-set record — advisory-for-UI, never an authz input.
                access: Some("writer".into()),
                // The identity root + cadence thread through the same DTO mapping.
                home_nest_actor_id: Some("ab".repeat(32)),
                welcome_bytes: vec![9, 8, 7],
                // The seal rides to the accept, which opens it once joined.
                set_name_seal: Some(label_custody::SealedSetName {
                    sealed: vec![0xED; 40],
                    name_hash: vec![0x5A; 32],
                }),
            }]
        );
        // A peek — listing never acks (accept/decline is a separate explicit ack).
        assert!(
            mock.acked.lock().unwrap().is_empty(),
            "listing pending shares must not ack"
        );
    }

    fn room_invite_env(signed: &[u8], node: Option<&str>) -> Vec<u8> {
        InboxEnvelope::room_invite(&fauna_protocol::inbox::RoomInviteInbox {
            signed_invite: signed.to_vec(),
            room_node: node.map(str::to_string),
            extra: Default::default(),
        })
        .unwrap()
        .to_canonical_bytes()
        .unwrap()
    }

    /// **A delivered room invitation is a knock: the drain never acks it.**
    ///
    /// Accepting is a decision only the user makes, so the row must survive
    /// every drain pass until an accept or a decline consumes it. The retention
    /// is the same mechanism as the generic unknown-kind retention: the invite
    /// stays un-acked until an accept or a decline consumes it.
    #[test]
    fn drain_retains_a_room_invitation_un_acked() {
        let mock = DrainMock::new(vec![InboxFetchReply {
            extra: Default::default(),
            items: vec![
                item(1, welcome_env("chan")),
                item(2, room_invite_env(&[0xaa, 0xbb], None)),
            ],
            more: false,
        }]);
        let client = InboxClient::new(mock.clone());
        let apply = RecordingApply::default();

        let outcome = block_on(drain(&client, &apply, 0)).expect("infallible transport");

        assert_eq!(outcome.applied, 1, "only the welcome is applied");
        assert_eq!(outcome.skipped, 1, "the invitation is retained");
        assert!(
            outcome.more_pending,
            "a standing invitation is undelivered residue by design"
        );
        assert_eq!(
            mock.acked.lock().unwrap().as_slice(),
            &[vec![1]],
            "the invitation's id is never acked by the drain"
        );
        assert_eq!(
            apply.applied.lock().unwrap().as_slice(),
            &["welcome:chan".to_string()],
            "there is no per-client apply hook for an invitation — nothing to apply"
        );
    }

    /// The peek returns every standing invitation and consumes none, ignoring
    /// the other kinds beside it.
    #[test]
    fn list_pending_room_invites_returns_only_invitations_without_acking() {
        let mock = DrainMock::new(vec![InboxFetchReply {
            extra: Default::default(),
            items: vec![
                item(1, welcome_env("dmchan")), // another kind — excluded
                item(2, room_invite_env(&[0x01, 0x02], None)),
                item(3, unknown_env()), // forward-compat — excluded
                item(4, vec![0x00]),    // undecodable — excluded
                item(9, room_invite_env(&[0x03], Some("https://other.example"))),
            ],
            more: false,
        }]);
        let client = InboxClient::new(mock.clone());

        let invites =
            block_on(list_pending_room_invites(&client, 0)).expect("infallible transport");

        assert_eq!(
            invites,
            vec![
                PendingRoomInvite {
                    inbox_id: 2,
                    signed_invite: vec![0x01, 0x02],
                    room_node: None,
                },
                PendingRoomInvite {
                    inbox_id: 9,
                    signed_invite: vec![0x03],
                    room_node: Some("https://other.example".into()),
                },
            ]
        );
        assert!(
            mock.acked.lock().unwrap().is_empty(),
            "listing invitations must not consume one"
        );
    }

    /// The walk pages past residue, for [`list_folder_pending_shares`]'s
    /// measured reason — and the residue an invitation hides behind is
    /// *guaranteed* here, because every OTHER standing invitation is un-ackable
    /// residue too.
    #[test]
    fn list_pending_room_invites_steps_past_a_full_page_of_residue() {
        let residue: Vec<_> = (1..=50).map(|i| item(i, unknown_env())).collect();
        let mock = DrainMock::new(vec![
            InboxFetchReply {
                extra: Default::default(),
                items: residue,
                more: true,
            },
            InboxFetchReply {
                extra: Default::default(),
                items: vec![item(51, room_invite_env(&[0xff], None))],
                more: false,
            },
        ]);
        let client = InboxClient::new(mock.clone());

        let invites =
            block_on(list_pending_room_invites(&client, 0)).expect("infallible transport");

        assert_eq!(
            invites,
            vec![PendingRoomInvite {
                inbox_id: 51,
                signed_invite: vec![0xff],
                room_node: None,
            }],
            "an invitation behind a page of residue is still discoverable"
        );
        assert!(mock.acked.lock().unwrap().is_empty());
    }

    /// `shared_by_display` folds the fallback rule into the DTO so no client
    /// re-derives it (the rule the four apps had drifted on): handle wins;
    /// a handle-less stamped share gets the canonical `short_id` truncation of
    /// the hex (12 chars + `…`); a fully unstamped cross-nest share is empty
    /// (the client renders its unknown-sharer i18n label).
    #[test]
    fn pending_share_display_falls_back_handle_then_short_id_then_empty() {
        let hex64 = "aa11bb22cc33dd44ee55ff6600112233445566778899aabbccddeeff00112233";
        let mock = DrainMock::new(vec![InboxFetchReply {
            extra: Default::default(),
            items: vec![
                item(
                    1,
                    folder_welcome_env("c1", "g1", Some(hex64), Some("carol")),
                ),
                item(2, folder_welcome_env("c2", "g2", Some(hex64), None)),
                item(3, folder_welcome_env("c3", "g3", None, None)),
            ],
            more: false,
        }]);
        let client = InboxClient::new(mock);

        let shares =
            block_on(list_folder_pending_shares(&client, 0)).expect("infallible transport");
        let displays: Vec<&str> = shares
            .iter()
            .map(|s| s.shared_by_display.as_str())
            .collect();
        assert_eq!(displays, vec!["carol", "aa11bb22cc33…", ""]);
    }

    /// A cross-nest share whose owner label the recipient's own nest verified
    /// (`federation.md` § … *The cross-nest owner label*) arrives with
    /// `shared_by` unstamped — the gate still knocks — and the pair beside it:
    /// the knock names the sharer by the canonical `handle@domain`, never a
    /// bare handle. One without the pair stays the empty unknown-sharer string.
    #[test]
    fn a_verified_cross_nest_sharer_is_labelled_handle_at_domain() {
        let stamped = InboxEnvelope::welcome(&WelcomeInbox {
            welcome_bytes: vec![1],
            channel_id: Some("c1".into()),
            channel_type: Some("folder".into()),
            group_id: Some("g1".into()),
            shared_by_handle: Some("alice".into()),
            shared_by_domain: Some("example.com".into()),
            ..Default::default()
        })
        .unwrap()
        .to_canonical_bytes()
        .unwrap();
        let mock = DrainMock::new(vec![InboxFetchReply {
            extra: Default::default(),
            items: vec![
                item(1, stamped),
                item(2, folder_welcome_env("c2", "g2", None, None)),
            ],
            more: false,
        }]);
        let client = InboxClient::new(mock);
        let shares =
            block_on(list_folder_pending_shares(&client, 0)).expect("infallible transport");
        assert_eq!(shares[0].shared_by_display, "alice@example.com");
        assert_eq!(shares[0].shared_by, None, "the label names, never admits");
        assert_eq!(shares[0].shared_by_domain.as_deref(), Some("example.com"));
        assert_eq!(shares[1].shared_by_display, "");
    }

    #[test]
    fn list_pending_shares_empty_when_no_folder_welcomes() {
        let mock = DrainMock::new(vec![InboxFetchReply {
            extra: Default::default(),
            items: vec![item(1, welcome_env("dmchan")), item(2, unknown_env())],
            more: false,
        }]);
        let client = InboxClient::new(mock.clone());

        let shares =
            block_on(list_folder_pending_shares(&client, 0)).expect("infallible transport");
        assert!(shares.is_empty());
    }

    #[test]
    fn drain_dispatches_each_kind_acks_applied_skips_rest() {
        let mock = DrainMock::new(vec![InboxFetchReply {
            extra: Default::default(),
            items: vec![
                item(1, welcome_env("chan")),
                item(
                    2,
                    InboxEnvelope::contact_request(vec![0xaa, 0xbb])
                        .to_canonical_bytes()
                        .unwrap(),
                ),
                item(
                    3,
                    InboxEnvelope::security_notice(&SecurityNoticeInbox {
                        extra: Default::default(),
                        subject: "Hi".into(),
                        body: "there".into(),
                    })
                    .unwrap()
                    .to_canonical_bytes()
                    .unwrap(),
                ),
                item(4, unknown_env()), // forward-compat skip
                item(5, vec![0x00]),    // undecodable skip (CBOR int, not a map)
            ],
            more: false,
        }]);
        let client = InboxClient::new(mock.clone());
        let apply = RecordingApply::default();

        let outcome = block_on(drain(&client, &apply, 0)).expect("infallible transport");

        assert_eq!(outcome.applied, 3);
        assert_eq!(outcome.skipped, 2);
        assert!(
            outcome.more_pending,
            "the 2 skipped rows are still undelivered residue, even though the \
             nest reported no further page"
        );
        // Acked exactly the three applied ids, in one ack call, in order.
        assert_eq!(mock.acked.lock().unwrap().as_slice(), &[vec![1, 2, 3]]);
        // Dispatched each kind to the right hook, in queue order.
        assert_eq!(
            apply.applied.lock().unwrap().as_slice(),
            &[
                "welcome:chan".to_string(),
                "contact_request:[170, 187]".to_string(),
                "security_notice:Hi".to_string(),
            ]
        );
    }

    #[test]
    fn drain_pages_while_making_progress() {
        let mock = DrainMock::new(vec![
            InboxFetchReply {
                extra: Default::default(),
                items: vec![item(1, welcome_env("a"))],
                more: true,
            },
            InboxFetchReply {
                extra: Default::default(),
                items: vec![item(2, welcome_env("b"))],
                more: false,
            },
        ]);
        let client = InboxClient::new(mock.clone());
        let apply = RecordingApply::default();

        let outcome = block_on(drain(&client, &apply, 0)).expect("infallible transport");

        assert_eq!(outcome.applied, 2);
        assert_eq!(outcome.skipped, 0);
        assert!(!outcome.more_pending);
        assert_eq!(*mock.fetch_count.lock().unwrap(), 2, "paged twice");
        assert_eq!(mock.acked.lock().unwrap().as_slice(), &[vec![1], vec![2]]);
    }

    #[test]
    fn drain_stops_on_all_skipped_page_no_infinite_loop() {
        // `DrainMock` repeats its last scripted page regardless of `after_id`,
        // so it models a nest that does **not** honor the skip cursor. The drain
        // advances the cursor once, sees the same head come back, and stops: two
        // fetches, never a spin. (Against a cursor-honoring nest the same queue
        // would instead be walked past — `drain_reaches_a_welcome_behind_a_full_
        // page_of_unackable_notices`.) If the guard were missing, this hangs.
        let mock = DrainMock::new(vec![InboxFetchReply {
            extra: Default::default(),
            items: vec![item(7, unknown_env())],
            more: true,
        }]);
        let client = InboxClient::new(mock.clone());
        let apply = RecordingApply::default();

        let outcome = block_on(drain(&client, &apply, 0)).expect("infallible transport");

        assert_eq!(outcome.applied, 0);
        assert_eq!(outcome.skipped, 1);
        assert!(outcome.more_pending, "residue reported for a future build");
        assert_eq!(
            *mock.fetch_count.lock().unwrap(),
            2,
            "one cursor probe, then stop on the non-advance — not a spin"
        );
        assert!(
            mock.acked.lock().unwrap().is_empty(),
            "never acked an unknown"
        );
    }

    // ── Head-of-line blocking past a page (the skip cursor) ─────────────────

    /// Models the **real** nest rather than a script: a durable queue of
    /// undelivered rows served oldest-first by delivery-link id
    /// (`bins/fauna-nest/src/db/inbox.rs::poll_inbox`), truncated to `limit`
    /// with a `more` sentinel and filtered by the skip cursor
    /// (`bins/fauna-nest/src/inbox_handlers.rs::fetch_handler`); `ack` removes
    /// rows. `DrainMock`'s scripted pages cannot express this test — a scripted
    /// second page would hand the drain the tail it is supposed to prove it can
    /// *reach*, passing even with the defect present.
    struct NestQueueMock {
        undelivered: Mutex<Vec<(i64, Vec<u8>)>>,
        /// `false` models a nest that ignores the cursor: `after_id` is ignored and
        /// the same head comes back (the client must not spin on it).
        honors_cursor: bool,
        fetch_count: Mutex<usize>,
    }

    impl NestQueueMock {
        fn new(rows: Vec<(i64, Vec<u8>)>, honors_cursor: bool) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                undelivered: Mutex::new(rows),
                honors_cursor,
                fetch_count: Mutex::new(0),
            })
        }
    }

    impl RpcRequester for NestQueueMock {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).unwrap();
            let reply_bytes = match kind {
                "fauna.inbox.fetch" => {
                    *self.fetch_count.lock().unwrap() += 1;
                    let req: InboxFetchRequest = fauna_protocol::decode_strict(&bytes).unwrap();
                    let limit = if req.limit == 0 { 100 } else { req.limit } as usize;
                    let rows = self.undelivered.lock().unwrap();
                    let after = if self.honors_cursor {
                        req.after_id
                    } else {
                        None
                    };
                    let mut visible: Vec<_> = rows
                        .iter()
                        .filter(|(id, _)| after.is_none_or(|a| *id > a))
                        .cloned()
                        .collect();
                    let more = visible.len() > limit;
                    visible.truncate(limit);
                    fauna_protocol::encode_canonical(&InboxFetchReply {
                        extra: Default::default(),
                        items: visible
                            .into_iter()
                            .map(|(id, payload)| item(id, payload))
                            .collect(),
                        more,
                    })
                    .unwrap()
                }
                "fauna.inbox.ack" => {
                    let req: InboxAckRequest = fauna_protocol::decode_strict(&bytes).unwrap();
                    let acked = req.ids.len() as u32;
                    self.undelivered
                        .lock()
                        .unwrap()
                        .retain(|(id, _)| !req.ids.contains(id));
                    fauna_protocol::encode_canonical(&InboxAckReply {
                        extra: Default::default(),
                        acked,
                    })
                    .unwrap()
                }
                other => panic!("NestQueueMock: unexpected kind {other}"),
            };
            Ok(fauna_protocol::decode_strict(&reply_bytes).unwrap())
        }
    }

    /// An apply with unackable heads: a Welcome applies; the notice kinds `Err`
    /// and are left un-acked. This WAS production's shape until 2026-08-10
    /// (both faces now ack security notices — the nest writes the rendered
    /// `notifications` row, `notifications.md` § Security notices); the
    /// scenario stays real and pinned because the head-of-line property was
    /// never notice-specific: an older client, a contact request, or an
    /// `InboxKind::Unknown` from a newer nest fills the head the same way.
    #[derive(Default)]
    struct SurfacelessNoticeApply {
        welcomes: Mutex<Vec<String>>,
    }

    impl InboxApply for SurfacelessNoticeApply {
        type Error = String;

        async fn apply_welcome(&self, w: WelcomeInbox) -> Result<(), Self::Error> {
            self.welcomes
                .lock()
                .unwrap()
                .push(w.channel_id.unwrap_or_default());
            Ok(())
        }

        async fn apply_contact_request(&self, _tuple: Vec<u8>) -> Result<(), Self::Error> {
            Err("no contact-request surface yet".into())
        }

        async fn apply_security_notice(&self, _n: SecurityNoticeInbox) -> Result<(), Self::Error> {
            Err("no security-notice surface yet".into())
        }
    }

    fn notice_env(subject: &str) -> Vec<u8> {
        InboxEnvelope::security_notice(&SecurityNoticeInbox {
            subject: subject.into(),
            body: "b".into(),
            ..Default::default()
        })
        .unwrap()
        .to_canonical_bytes()
        .unwrap()
    }

    #[test]
    fn drain_reaches_a_welcome_behind_a_full_page_of_unackable_notices() {
        // The defect this pins: `NewTokenIssued` fires on every new-IP sign-in,
        // no app renders a security notice, so the notices accumulate un-acked
        // at the head. Once they fill a whole page the drain sees zero acks,
        // stops, and the missed-push MLS-Welcome backstop behind them is
        // unreachable — silently and permanently.
        let mut rows: Vec<(i64, Vec<u8>)> = (1..=100)
            .map(|i| (i, notice_env(&format!("notice {i}"))))
            .collect();
        rows.push((101, welcome_env("rescued")));
        let mock = NestQueueMock::new(rows, true);
        let client = InboxClient::new(mock.clone());
        let apply = SurfacelessNoticeApply::default();

        let outcome = block_on(drain(&client, &apply, 100)).expect("infallible transport");

        assert_eq!(
            apply.welcomes.lock().unwrap().as_slice(),
            &["rescued".to_string()],
            "the Welcome behind 100 un-ackable notices must still be applied"
        );
        assert_eq!(outcome.applied, 1);
        assert_eq!(
            outcome.skipped, 100,
            "every notice left un-acked, none lost"
        );
        assert!(
            outcome.more_pending,
            "the un-ackable notices are still undelivered residue"
        );
        // The notices stay in the queue — no ack-and-discard (`critical-alerts.md`
        // § Mechanism: acking would consume a notice no app renders).
        assert_eq!(mock.undelivered.lock().unwrap().len(), 100);
    }

    #[test]
    fn drain_does_not_spin_against_a_nest_that_ignores_the_cursor() {
        // A client sends `after_id` to a nest that ignores it,
        // so the same head returns forever. The drain must stop, not hang.
        // 150 rows against a 100-item page, so `more` is genuinely true and the
        // drain has a reason to page again — the spin risk this pins.
        let rows: Vec<(i64, Vec<u8>)> = (1..=150)
            .map(|i| (i, notice_env(&format!("notice {i}"))))
            .collect();
        let mock = NestQueueMock::new(rows, false);
        let client = InboxClient::new(mock.clone());
        let apply = SurfacelessNoticeApply::default();

        let outcome = block_on(drain(&client, &apply, 100)).expect("infallible transport");

        assert_eq!(outcome.applied, 0);
        assert!(outcome.more_pending);
        assert!(
            *mock.fetch_count.lock().unwrap() <= 2,
            "must detect the ignored cursor after one retry, got {} fetches",
            *mock.fetch_count.lock().unwrap()
        );
    }

    /// An apply that always fails — the shape a **retired** `MlsEngine`
    /// produces once the Welcome doors are quiesce-guarded: the engine answers
    /// `MlsError::Retired`, `ingest_welcome` propagates it with `?` into a
    /// `BackendError`, and `SessionInboxApply::apply_welcome` hands it here as
    /// an `Err`.
    #[derive(Default)]
    struct RefusingApply {
        seen: Mutex<usize>,
    }

    impl InboxApply for RefusingApply {
        type Error = String;

        async fn apply_welcome(&self, _w: WelcomeInbox) -> Result<(), Self::Error> {
            *self.seen.lock().unwrap() += 1;
            Err("mls engine refused: engine retired".to_string())
        }

        async fn apply_contact_request(&self, _tuple: Vec<u8>) -> Result<(), Self::Error> {
            Ok(())
        }

        async fn apply_security_notice(&self, _n: SecurityNoticeInbox) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    /// **A refused Welcome is RETAINED, never acked** — the seam that turns the
    /// retired-engine ghost into a permanent loss, or does not.
    ///
    /// A retired engine used to join from a Welcome successfully: the join
    /// mutated only the in-memory provider, so the poisoned store never saw it,
    /// `persist_group` swallowed both its failures as `warn!`, and the apply
    /// returned `Ok`. This layer reads `Ok` as *"durably applied — ack it"*
    /// (`apply_one`'s contract), so the nest dropped the durable row. The
    /// successor then never received the Welcome, and it cannot be replayed:
    /// the user is a member of a group **no engine of theirs can open**, which
    /// is `nest/common.md` § Client-state recoverability, not an annoyance.
    ///
    /// So guarding the engine door is only half the fix — the refusal has to
    /// arrive here as `Err`. An `is_retired` check that returned `Ok(())`
    /// instead would ack-and-drop, strictly worse than the ghost it replaced.
    /// This pins the half the engine test cannot see: `Err` ⇒ **no ack call at
    /// all**, the row still queued for the successor.
    #[test]
    fn a_refused_welcome_is_never_acked_so_the_successor_still_gets_it() {
        let mock = DrainMock::new(vec![InboxFetchReply {
            extra: Default::default(),
            items: vec![item(41, welcome_env("cafe"))],
            more: false,
        }]);
        let client = InboxClient::new(mock.clone());
        let apply = RefusingApply::default();

        let outcome = block_on(drain(&client, &apply, 0)).expect("infallible transport");

        assert_eq!(
            *apply.seen.lock().unwrap(),
            1,
            "the Welcome reached the apply"
        );
        // Bind once: re-locking inside the failure message would deadlock.
        let acked = mock.acked.lock().unwrap().clone();
        assert!(
            acked.is_empty(),
            "a refused Welcome must leave NO ack call — acking it drops the nest's \
             durable row and the successor never receives the Welcome (it cannot be \
             replayed); got {acked:?}"
        );
        assert_eq!(outcome.applied, 0);
        assert_eq!(outcome.skipped, 1, "counted as skipped, not applied");
        assert!(
            outcome.more_pending,
            "the un-acked residue must be reported so the caller knows work remains"
        );
    }
}
