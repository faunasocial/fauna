//! The feed page loop, once — the paging law every `fauna.sync.changes.list`
//! walk in this crate obeys.
//!
//! Authority for the law itself: `docs/goal/architecture/account-sync-plane.md`
//! § Feeds and cursors. This module owns only its *mechanics*, which four
//! walks had each written out longhand:
//!
//! * [`crate::account_state_plane::AccountStatePlane::walk`] / `reconcile` —
//!   the class-2 account plane.
//! * [`crate::group_state_plane::GroupStatePlane::walk`] / `reconcile` — the
//!   storage-group sibling.
//! * [`crate::content_scope_plane::ContentScopePlane::walk`] / `reconcile` —
//!   the class-1 leg.
//! * `fauna_sync_engine::custody_leg::custody_pull` — the custodian's verbatim relay
//!   pull, whose own comments called itself "mirrored from the walk" and its
//!   spin refusal "the walk's spin refusal, verbatim".
//!
//! # The law
//!
//! 1. Ask the feed for a page from `cursor`.
//! 2. An **empty** page means converged — return the report. (A walk that
//!    finds nothing still costs one request; the empty page is not counted.)
//!    Its reply can still move the cursor — the serve-order watermark's echo
//!    on an empty page is the scope's tip — and a cursor it moved is
//!    checkpointed before the walk returns.
//! 3. Otherwise count the page, snapshot the cursor, apply every row, then let
//!    the cursor take the reply's own paging metadata
//!    ([`PageCursor::absorb_reply`]: the watermark's echo, which projects the
//!    writers the next request names).
//! 4. If the page moved the cursor **nowhere**, refuse to spin: the next
//!    request would fetch the same page forever, so a caller needs to see the
//!    row shape that caused it, not a loop that never returns.
//! 5. If the cursor has outgrown its shape's ceiling
//!    ([`PageCursor::overgrown`]), refuse — like step 4, this rejects the
//!    cursor, so it sits above the checkpoint.
//! 6. Optionally checkpoint.
//! 7. If the walk has spent its [`MAX_PAGES_PER_WALK`] budget, refuse — this
//!    rejects only the *walk*, so it sits below the checkpoint. Otherwise page
//!    again.
//!
//! Steps 5 and 7 exist because step 4 answers *"did this page move us?"* and
//! cannot answer *"is this feed ever going to end?"*.
//!
//! Step 4 is why this module exists. The four copies had already drifted in
//! *how* they asked it — three compared cursors for equality, the fourth (a
//! single-writer scope, whose cursor is a bare seq) for strict advance — and a
//! reader had no way to see that the difference is a property of the **cursor
//! shape**, not a per-plane choice. [`PageCursor`] is that difference, named.
//!
//! # Why step 4 is not a bound (the step-5 budgets)
//!
//! The spin refusal is a **stall detector**, and a stall is what an honest
//! feed does when it is broken. It says nothing about a feed that advances by
//! the *minimum* on every page, forever — and on this crate's walks the feed
//! is not always honest: `fauna_sync_engine::custody_leg::custody_pull` pages an
//! **admitted peer channel to an owner device**, so the rows, their count and
//! their coordinates are all the counterpart's to choose. A custodian is
//! key-less by design and cannot verify that a row's `origin_writer` names any
//! real device; the relay plane admits coordinate-valid rows unconditionally,
//! on purpose.
//!
//! Two things were therefore unbounded, and step 5 bounds them separately
//! because they are different costs:
//!
//! * **The walk itself never returns** ([`MAX_PAGES_PER_WALK`]). A feed that
//!   advances one seq per page satisfies step 4 on every page. Both cursor
//!   shapes have this shape of hole.
//! * **The frontier grows without limit** ([`MAX_FRONTIER_WRITERS`]). For the
//!   multi-writer shape a *fresh writer key* is progress under step 4 — the
//!   applies insert the key before validating anything, which is correct
//!   (the row was seen) but free to the counterpart. The frontier rides in
//!   **every subsequent request**, so unbounded growth is quadratic wire; the
//!   custodian's pull also persists it per page, so it is quadratic disk and
//!   an ever-larger durable blob. Left alone it ends at a cursor too large to
//!   send — a *stored* cursor whose walk can never run again, which is the
//!   client-causable unrecoverable state
//!   ([`nest/common.md`](nest/common.md) § Client-state recoverability) rather
//!   than merely a slow walk.
//!
//! Neither budget is a wall clock, deliberately. This driver is runtime-
//! agnostic — it takes no `Send` bound (see [`drive`]) precisely so each
//! [`fauna_protocol::RpcRequester`] impl keeps inferring its own, and it names
//! no timer, so no caller is tied to one. `PeerChannel::request` already puts
//! the deadline on the **caller** ("no deadline is attached — the caller can
//! race this against its own timeout"), and that is where it stays: a walk
//! that should not run for an hour is bounded by `peer_leg`'s
//! `PER_SIBLING_BUDGET` or `custody_leg`'s `PER_CUSTODY_OWNER_BUDGET`. This
//! module guarantees only the thing no timeout can — that the loop
//! *terminates* — which is what makes those budgets a latency ceiling rather
//! than the only thing standing between a walk and forever.
//!
//! # What stays with the caller
//!
//! Everything plane-shaped: the request (item class, `since` vs `frontier`,
//! `of_owner`), the rpc error wording, the per-row apply, and any per-walk
//! memo — which lives in the apply closure's own captures, so a plane that
//! threads one (the account plane's per-generation schedule memo) and a plane
//! that does not are the same call here.

use anyhow::{Result, bail};
use fauna_protocol::sync::{SyncChange, SyncChangesListReply};
use std::collections::BTreeMap;

/// How many pages one walk will accept before it refuses to keep going.
///
/// The termination guarantee, not a performance knob — sized so no honest walk
/// can reach it. A peer serves at most `fauna_peer_sync::MAX_ROWS_PER_PAGE`
/// (64) rows per page, so this is 4.2 M rows for a single scope, well past the
/// live-entry set any replica holds; the nest leg's pages are byte-budgeted
/// (`SERVE_PAGE_BUDGET_BYTES`, ~2 MiB) and therefore denser still, so its
/// honest page count is lower again.
///
/// Refusing is safe to reach because a walk is **resumable**: the class-2
/// planes keep this cursor in memory only and re-derive it from the store's
/// durable frontier, which advanced for every row actually held, and the
/// custodian's pull checkpoints per page below the refusal. A refused walk has
/// therefore banked its progress and simply ends the pass, exactly as an
/// unreachable peer does.
const MAX_PAGES_PER_WALK: usize = 65_536;

/// How many distinct writers one scope's frontier may name.
///
/// The honest ceiling is small and structural: an account's live device
/// writers (`AdminTier::max_devices` — 3, 5 and 10 in the shipped tiers, at
/// most 64 by admin setting), plus the retired identities the store keeps
/// **permanently** (account-replica-posture.md § The store device principal →
/// *Principal succession after a device delete*, refinement 8 — one per
/// succession), plus the nest, plus — on a shared-set scope — the same again
/// for every member. This
/// ceiling is orders of magnitude past all of that, and still keeps the
/// frontier a request carries (64-char hex keys) around 300 KB, a fraction of
/// the 2 MiB WS frame every request must fit.
///
/// The precedent for answering a counterpart-driven unbounded loop with a
/// hard-coded cap rather than a heuristic is the rotation cap of
/// account-replica-posture.md § The store device principal →
/// *Principal succession after a device delete*, refinement 7.
///
/// **Public because the nest sizes the admin-settable `AdminTier::max_devices`
/// bound against it** (`admin_ws_handlers::MAX_TIER_MAX_DEVICES`). The doc
/// comment above derives this ceiling from the honest writer set, whose first
/// term is `max_devices` — for years nothing in the tree bound that term or
/// even named the relationship, so the derivation was an assertion about a
/// number an admin could set to anything. Deriving the tier bound from this
/// constant is what makes the sentence above true of the code that ships;
/// the terms that remain unbounded are named in
/// `account-sync-plane.md` § Feeds and cursors. The compaction that ceiling
/// owes an honest frontier is the serve-order watermark ruled there, built on
/// the nest leg (`WatermarkCursor`): against a nest that honours it a
/// request names only the writers the watermark cannot cover, so an honest
/// frontier no longer approaches this ceiling there. The store-served legs
/// still send the whole frontier, and on them it stays the guard it was.
pub const MAX_FRONTIER_WRITERS: usize = 4_096;

/// A paging cursor, in the two shapes the feed's walks carry.
///
/// The trait exists for one question — *did this page move us?* — which the
/// two shapes answer differently and for a reason, not by accident:
///
/// * A **multi-writer** frontier (`{writer: seq}`) advances when *any* slot
///   moves, so the test is inequality. A page is progress even when it moved
///   one writer backwards-looking slots and left the rest alone, because the
///   next request's frontier is then a different question.
/// * A **single-writer** scope's cursor is one nest sequence number, and the
///   next request sends it as `since`. Only a strict increase changes the
///   question, so equality is a stall exactly like a decrease would be.
pub trait PageCursor: Clone {
    /// Did the page just applied move this cursor past `before`?
    fn advanced_from(&self, before: &Self) -> bool;

    /// The stall message's tail, spoken about `self` — the **pre-page**
    /// cursor, which is why the single-writer shape can name the sequence it
    /// failed to get past. [`drive_checkpointed`] prefixes the caller's label.
    fn stall_detail(&self, rows: usize) -> String;

    /// `Some(detail)` when this cursor has outgrown what its shape can carry —
    /// the refusal's tail, like [`Self::stall_detail`].
    ///
    /// A ceiling only exists where the cursor's *size* is the counterpart's to
    /// choose, which is a property of the shape: a frontier grows one entry per
    /// writer named by a row, a bare sequence number does not grow at all.
    fn overgrown(&self) -> Option<String>;

    /// Take the reply's own paging metadata once its rows are applied — the
    /// serve-order watermark's echo ([`WatermarkCursor`]). Nothing by default:
    /// a cursor that does not page by serve order ignores an echo, so the
    /// walk keeps its plain frontier contract.
    fn absorb_reply(&mut self, _reply: &SyncChangesListReply) {}

    /// Did absorbing a reply since `before` VOID the watermark this cursor
    /// paged by (`account-sync-plane.md` § The bind leg, ruling 2)? Then that
    /// reply answered a request gated by a watermark that meant nothing to its
    /// nest, so an empty page is not convergence and the walk pages on.
    /// `false` by default: a cursor with no watermark has none to void.
    fn voided_since(&self, _before: &Self) -> bool {
        false
    }
}

impl PageCursor for BTreeMap<String, i64> {
    fn advanced_from(&self, before: &Self) -> bool {
        self != before
    }

    fn stall_detail(&self, rows: usize) -> String {
        format!("a page of {rows} rows advanced no writer cursor")
    }

    fn overgrown(&self) -> Option<String> {
        (self.len() > MAX_FRONTIER_WRITERS).then(|| {
            format!(
                "the frontier names {} writers, past the {MAX_FRONTIER_WRITERS} a scope can have",
                self.len(),
            )
        })
    }
}

impl PageCursor for i64 {
    fn advanced_from(&self, before: &Self) -> bool {
        self > before
    }

    fn stall_detail(&self, rows: usize) -> String {
        format!("a page of {rows} rows advanced the cursor past {self}")
    }

    /// A single-writer cursor is one number: there is nothing for a
    /// counterpart to grow, so this shape has no ceiling to outgrow.
    fn overgrown(&self) -> Option<String> {
        None
    }
}

/// A multi-writer paging cursor, as a class-2 walk drives it: something to
/// raise as rows arrive, and the two request fields it sends.
///
/// Two shapes. The whole frontier (`BTreeMap`) is every store-served leg's
/// cursor; [`WatermarkCursor`] is the nest leg's, which pages by the nest's
/// serve order once the nest has echoed one.
pub trait FrontierCursor: PageCursor {
    /// The page just showed `writer_hex`'s row at that writer's own `seq`.
    fn saw(&mut self, writer_hex: String, seq: i64);

    /// The next request's `frontier`.
    fn frontier(&self) -> &BTreeMap<String, i64>;

    /// The next request's `held_through_seq`.
    fn held_through_seq(&self) -> Option<i64>;

    /// A previously banked watermark went unechoed this walk — the caller
    /// must clear the *persisted* watermark
    /// ([`fauna_account_store::store::AccountStore::clear_nest_watermark`])
    /// rather than merely skip raising it, or the next walk reopens narrow
    /// against the same non-honouring nest and repeats the same full
    /// re-serve forever (account-sync-plane.md § Feeds and cursors). `false`
    /// by default: a store-served cursor is never banked in the first place.
    fn watermark_dishonoured(&self) -> bool {
        false
    }

    /// The replica id the watermark [`Self::held_through_seq`] names is keyed
    /// to — what the caller banks it under
    /// ([`fauna_account_store::store::AccountStore::raise_nest_watermark`];
    /// `account-sync-plane.md` § The bind leg, ruling 2). `None` by default:
    /// a store-served cursor has no watermark to key.
    fn replica_id(&self) -> Option<&[u8]> {
        None
    }

    /// Would `reply` find this cursor's banked watermark void — the verdict
    /// [`PageCursor::absorb_reply`] reaches, asked BEFORE the reply's rows
    /// are applied, so the caller can void what rides with the bank (every
    /// relay row's serve coordinate, `account-sync-plane.md` § The bind leg,
    /// ruling 2) before the new replica's first row is recorded. `false` by
    /// default: a store-served cursor banks nothing.
    fn voids_bank(&self, _reply: &SyncChangesListReply) -> bool {
        false
    }
}

impl FrontierCursor for BTreeMap<String, i64> {
    fn saw(&mut self, writer_hex: String, seq: i64) {
        let slot = self.entry(writer_hex).or_insert(0);
        *slot = (*slot).max(seq);
    }

    fn frontier(&self) -> &BTreeMap<String, i64> {
        self
    }

    /// The whole frontier is the store-served legs' shape, and a store serves
    /// in `(writer, writer_seq)` order — no cross-writer order a watermark
    /// could be a coordinate in.
    fn held_through_seq(&self) -> Option<i64> {
        None
    }
}

/// The nest leg's paging cursor under the serve-order watermark
/// (`account-sync-plane.md` § Feeds and cursors → *Compaction is a serve-order
/// watermark*): the `held_through_seq` a request sends, and the writers its
/// `frontier` still names.
///
/// Until the nest echoes a `complete_through_seq` this is the whole frontier
/// in all but name: every row raises it, nothing leaves it, and the ceiling
/// refuses it exactly as before — which is all a walk never echoed (the peer
/// leg) gets. Each echo ([`PageCursor::absorb_reply`]) raises the watermark and
/// **projects** the named writers down to the pinned ones: a writer the
/// watermark now covers need not be named, because an unnamed writer is
/// served from above the watermark rather than from 0. The projection lands
/// after the page's applies and before the refusals, so a single page naming
/// more writers than the ceiling is paged rather than refused — and it is a
/// projection of what the walk SENDS: the store's frontier, the accounting
/// law's record, is never touched by it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatermarkCursor {
    held_through_seq: Option<i64>,
    named: BTreeMap<String, i64>,
    /// `frontier` ∪ `pinned` as the walk opened — kept even when a banked
    /// watermark starts `named` narrow, purely as the fallback
    /// [`Self::absorb_reply`] restores `named` to the moment a bank goes
    /// unechoed: a nest that stops echoing it (the peer leg, or a nest
    /// restored from a backup) must be answered with real per-writer slots, not
    /// an empty frontier that reads, to a nest that echoes no watermark, as
    /// "every writer is new" (account-sync-plane.md § Feeds and cursors).
    whole: BTreeMap<String, i64>,
    /// The writers every projection keeps naming — the ones whose naming
    /// carries a duty the watermark cannot (a row seen but never accounted,
    /// which the nest must keep re-presenting). Fixed for the walk.
    pinned: std::collections::BTreeSet<String>,
    /// Whether THIS walk has independently reconfirmed the bank with an echo
    /// of its own. `false` only for a cursor [`Self::open`]ed against an
    /// already-persisted watermark, until this walk's own first reply
    /// resolves it one way or the other. A bank this walk has already
    /// reconfirmed is never undone by a later reply carrying no echo, or a
    /// negative one — same as before this fix, and the reason: one glitchy
    /// reply from a nest already proven to honour the watermark this walk is
    /// not evidence it stopped, unlike an opening reply that never confirms
    /// what `open` merely inherited from a previous walk.
    confirmed: bool,
    /// Set the first time a banked watermark is found void — see
    /// [`FrontierCursor::watermark_dishonoured`].
    dishonoured: bool,
    /// How many times this walk found its watermark void — what
    /// [`PageCursor::voided_since`] compares.
    voids: u32,
    /// The replica id [`Self::held_through_seq`] is keyed to
    /// (`account-sync-plane.md` § The bind leg, ruling 2): the one the bank
    /// was recorded against when [`Self::open`]ed from one, else the one
    /// named by the reply whose echo set it. A reply naming any other — or
    /// none where this is set — voids the watermark whatever it echoes.
    replica: Option<Vec<u8>>,
}

impl WatermarkCursor {
    /// A walk's opening cursor. `frontier` is what the walk would have sent
    /// whole; `pinned` maps each writer a projection must keep naming to the
    /// slot it is named at.
    ///
    /// With a watermark already banked, the request names the pinned writers
    /// alone. Without one it names the whole frontier with the pinned writers
    /// folded in: nothing is projected before the nest has echoed.
    /// `banked_replica` is the replica id the bank was recorded against
    /// ([`fauna_account_store::store::AccountStore::nest_watermark_replica`]).
    pub fn open(
        held_through_seq: Option<i64>,
        banked_replica: Option<Vec<u8>>,
        frontier: BTreeMap<String, i64>,
        pinned: BTreeMap<String, i64>,
    ) -> Self {
        let mut whole = frontier;
        for (writer, slot) in &pinned {
            whole.entry(writer.clone()).or_insert(*slot);
        }
        let named = match held_through_seq {
            Some(_) => pinned.clone(),
            None => whole.clone(),
        };
        Self {
            confirmed: held_through_seq.is_none(),
            held_through_seq,
            named,
            whole,
            pinned: pinned.into_keys().collect(),
            dishonoured: false,
            voids: 0,
            replica: held_through_seq.and(banked_replica),
        }
    }
}

impl PageCursor for WatermarkCursor {
    /// The next request is a different question when either field it sends
    /// moved: a higher watermark, or a named slot that rose or left.
    fn advanced_from(&self, before: &Self) -> bool {
        self.held_through_seq != before.held_through_seq || self.named != before.named
    }

    fn stall_detail(&self, rows: usize) -> String {
        let named = self.named.stall_detail(rows);
        match self.held_through_seq {
            Some(held) => format!("{named} and no watermark past {held}"),
            None => named,
        }
    }

    /// Only what the request names can outgrow a frame: the watermark is one
    /// number, however much history it covers.
    fn overgrown(&self) -> Option<String> {
        self.named.overgrown()
    }

    fn absorb_reply(&mut self, reply: &SyncChangesListReply) {
        if self.voids_bank(reply) {
            // Fall back to naming every writer for the rest of this walk,
            // exactly as a walk that never saw an echo would, merging in
            // anything already seen so a writer new to this walk (not in
            // `whole`) keeps its advanced slot. This reply's echo answered
            // a request gated by the void watermark, so it banks nothing;
            // the next reply's does, keyed to whichever replica sent it.
            let mut restored = self.whole.clone();
            for (writer, seq) in std::mem::take(&mut self.named) {
                restored.saw(writer, seq);
            }
            self.named = restored;
            self.held_through_seq = None;
            self.replica = None;
            self.confirmed = false;
            self.dishonoured = true;
            self.voids += 1;
            return;
        }
        let Some(complete) = watermark_echo(reply) else {
            return;
        };
        self.held_through_seq = Some(
            self.held_through_seq
                .map_or(complete, |held| held.max(complete)),
        );
        self.replica = reply.replica_id.as_ref().map(|id| id.to_vec());
        self.confirmed = true;
        let pinned = &self.pinned;
        self.named.retain(|writer, _| pinned.contains(writer));
    }

    fn voided_since(&self, before: &Self) -> bool {
        self.voids > before.voids
    }
}

/// A reply's `complete_through_seq`: a negative echo is no coordinate in any
/// log, and is taken as none.
fn watermark_echo(reply: &SyncChangesListReply) -> Option<i64> {
    reply.complete_through_seq.filter(|seq| *seq >= 0)
}

impl FrontierCursor for WatermarkCursor {
    fn saw(&mut self, writer_hex: String, seq: i64) {
        self.named.saw(writer_hex, seq);
    }

    fn frontier(&self) -> &BTreeMap<String, i64> {
        &self.named
    }

    fn held_through_seq(&self) -> Option<i64> {
        self.held_through_seq
    }

    fn watermark_dishonoured(&self) -> bool {
        self.dishonoured
    }

    fn replica_id(&self) -> Option<&[u8]> {
        self.replica.as_deref()
    }

    fn voids_bank(&self, reply: &SyncChangesListReply) -> bool {
        let echo = watermark_echo(reply);
        let replica = reply.replica_id.as_ref().map(|id| id.to_vec());
        if let Some(held) = self.held_through_seq {
            // The watermark is void (`account-sync-plane.md` § The bind leg,
            // ruling 2) when the reply comes from another replica than the
            // one it is keyed to — another id, or none where one is set:
            // a second nest, or a box rebuilt under the same identity, whose
            // log it says nothing about — at any point in the walk. And, on
            // the opening reply of a walk that merely INHERITED the bank, when
            // the nest does not confirm it: no echo is a nest that stopped
            // echoing the watermark (the peer leg, or a restored backup),
            // and an echo below it is a log that does not reach it — without
            // this, a non-empty second nest "confirms" a stale bank by echoing
            // its own lower tip, and every row between is skipped silently.
            // A bank this walk already reconfirmed is not undone by one
            // echo-less reply from the same replica: that is a glitch, not
            // evidence.
            let other_replica = replica != self.replica;
            let unconfirmed = !self.confirmed && echo.is_none_or(|seq| seq < held);
            return other_replica || unconfirmed;
        }
        false
    }
}

/// A walk report that counts pages. The counter's width is the report's own
/// business (`usize` on the state-entry walks, `u32` on the class-1 one), which
/// is the whole reason this is a trait and not a field the driver touches.
pub trait PageTally {
    /// One non-empty page was fetched.
    fn page_fetched(&mut self);
}

/// Drive the page loop to convergence.
///
/// `label` prefixes the spin refusal — pass the walk's own name exactly as it
/// should read (`"account-state walk"`, `"custody pull (state)"`); the tail
/// comes from [`PageCursor::stall_detail`].
///
/// `fetch` builds and sends one request from the current cursor; `apply` folds
/// one row into the report and the cursor. Neither is given a `Send` bound, on
/// purpose: [`fauna_protocol::RpcRequester`] is AFIT precisely so each impl
/// infers its own (native `Send`, wasm `!Send`), and a bound here would defeat
/// that for every caller.
///
/// # Errors
/// Whatever `fetch` or `apply` return, or the refusals of steps 4 and 5.
pub async fn drive<C, R>(
    cursor: C,
    report: R,
    label: &str,
    fetch: impl AsyncFn(&C) -> Result<SyncChangesListReply>,
    apply: impl AsyncFnMut(&SyncChange, &mut R, &mut C) -> Result<()>,
) -> Result<R>
where
    C: PageCursor,
    R: PageTally,
{
    drive_checkpointed(cursor, report, label, fetch, apply, async |_: &C| Ok(())).await
}

/// [`drive`], plus a hook that runs after every page the loop accepts.
///
/// The custodian's pull persists its cursor there, so a crashed pull resumes
/// at the last page rather than at the top.
///
/// It runs after the refusals that reject *the cursor* — the spin refusal
/// (a page that moved nothing is never banked as progress) and the
/// overgrown-frontier one, which keeps the durable blob at the last page that
/// was under the ceiling, so the stored cursor stays **sendable** and the pull
/// resumes on its own once the counterpart stops feeding fresh writers, or the
/// custody grant is revoked from the owner's app. That is the user-facing
/// remedy; a meta row deleted by hand is not one.
///
/// It runs *before* the page budget, which rejects only the **walk**: that
/// cursor is honest progress, and a pass that ends there must hand the next
/// one everything it earned.
///
/// # Errors
/// As [`drive`], plus whatever `after_page` returns.
pub async fn drive_checkpointed<C, R>(
    mut cursor: C,
    mut report: R,
    label: &str,
    fetch: impl AsyncFn(&C) -> Result<SyncChangesListReply>,
    mut apply: impl AsyncFnMut(&SyncChange, &mut R, &mut C) -> Result<()>,
    mut after_page: impl AsyncFnMut(&C) -> Result<()>,
) -> Result<R>
where
    C: PageCursor,
    R: PageTally,
{
    let mut pages = 0usize;
    loop {
        let reply = fetch(&cursor).await?;
        if reply.changes.is_empty() {
            // Converged. An empty page can still move the cursor — the
            // watermark's echo on one is the scope's tip, which is how a
            // converged walk banks the whole log — so a cursor it moved is
            // checkpointed. One it did not (every other shape) is not.
            let before = cursor.clone();
            cursor.absorb_reply(&reply);
            if cursor.advanced_from(&before) {
                after_page(&cursor).await?;
            }
            if cursor.voided_since(&before) {
                continue;
            }
            return Ok(report);
        }
        report.page_fetched();
        pages += 1;
        let before = cursor.clone();
        for change in &reply.changes {
            apply(change, &mut report, &mut cursor).await?;
        }
        // The reply's own paging metadata: AFTER the applies, because it
        // vouches for the page they just took, and BEFORE the refusals below,
        // so both judge the cursor the next request will actually send.
        cursor.absorb_reply(&reply);
        if !cursor.advanced_from(&before) {
            bail!("{label}: {}", before.stall_detail(reply.changes.len()));
        }
        // Step 5a — the cursor itself is the thing that outgrew its shape, so
        // this refusal sits with the spin refusal, ABOVE the checkpoint: the
        // durable blob must not learn a frontier the next request could not
        // send.
        if let Some(detail) = cursor.overgrown() {
            bail!("{label}: {detail}");
        }
        after_page(&cursor).await?;
        // Step 5b — the budget, BELOW the checkpoint, and that asymmetry is
        // the point: this cursor is honest progress the walk simply ran out of
        // pages to extend, so the next pass must start from it rather than
        // re-fetch the page that spent the last of the budget.
        if pages >= MAX_PAGES_PER_WALK {
            bail!(
                "{label}: refusing to page past the {MAX_PAGES_PER_WALK}-page budget \
                 — the feed advances but never converges"
            );
        }
        // A page's applies are one unit of local work: every store call
        // under them is synchronous, so without this a walk over an instant
        // or local feed holds the store thread for the whole log
        // (`pass_breath` module docs).
        crate::pass_breath::pass_breath().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::sync::SyncChange;

    #[derive(Debug, Default)]
    struct Tally {
        pages: usize,
        rows: usize,
    }

    impl PageTally for Tally {
        fn page_fetched(&mut self) {
            self.pages += 1;
        }
    }

    fn row(seq: i64) -> SyncChange {
        SyncChange {
            seq,
            ..Default::default()
        }
    }

    fn page(rows: Vec<SyncChange>) -> SyncChangesListReply {
        SyncChangesListReply {
            changes: rows,
            ..Default::default()
        }
    }

    /// The empty page is convergence, and it is not counted.
    #[tokio::test]
    async fn an_empty_first_page_converges_without_counting_it() {
        let report = drive(
            0i64,
            Tally::default(),
            "test walk",
            async |_: &i64| Ok(page(vec![])),
            async |_: &SyncChange, _: &mut Tally, _: &mut i64| Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(report.pages, 0);
        assert_eq!(report.rows, 0);
    }

    /// The single-writer shape: a page whose rows leave the cursor exactly
    /// where it was refuses loudly rather than re-fetching it forever. Only
    /// `custody_leg`'s own pull pinned this path before the driver existed.
    #[tokio::test]
    async fn a_seq_cursor_page_that_advances_nothing_refuses_loudly() {
        let err = drive(
            7i64,
            Tally::default(),
            "content-scope walk (media)",
            async |_: &i64| Ok(page(vec![row(3), row(5)])),
            // Seen, but behind the cursor: `max` leaves it at 7.
            async |change: &SyncChange, report: &mut Tally, cursor: &mut i64| {
                report.rows += 1;
                *cursor = (*cursor).max(change.seq);
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "content-scope walk (media): a page of 2 rows advanced the cursor past 7",
        );
    }

    /// A seq cursor that moved *backwards* is a stall, not progress — the one
    /// case where the single-writer predicate (strict advance) and the
    /// multi-writer one (inequality) disagree, and the reason
    /// `PageCursor for i64` is a strict `>`: the next request would send the
    /// lower `since` and be answered with the same page, forever.
    #[tokio::test]
    async fn a_seq_cursor_that_moved_backwards_is_a_stall_not_progress() {
        let err = drive(
            7i64,
            Tally::default(),
            "content-scope walk (media)",
            async |_: &i64| Ok(page(vec![row(2)])),
            async |change: &SyncChange, report: &mut Tally, cursor: &mut i64| {
                report.rows += 1;
                *cursor = change.seq;
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "content-scope walk (media): a page of 1 rows advanced the cursor past 7",
        );
    }

    /// The multi-writer shape refuses on the same law, and names it in the
    /// frontier's own vocabulary.
    #[tokio::test]
    async fn a_frontier_page_that_advances_no_writer_refuses_loudly() {
        let err = drive(
            BTreeMap::from([("aa".to_string(), 4i64)]),
            Tally::default(),
            "account-state walk",
            async |_: &BTreeMap<String, i64>| Ok(page(vec![row(1)])),
            async |_: &SyncChange, report: &mut Tally, _: &mut BTreeMap<String, i64>| {
                report.rows += 1;
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "account-state walk: a page of 1 rows advanced no writer cursor",
        );
    }

    /// A cursor that moves keeps paging, and the checkpoint fires once per
    /// accepted page — never for the empty page that ends the walk.
    #[tokio::test]
    async fn the_checkpoint_fires_once_per_accepted_page() {
        let mut checkpoints: Vec<i64> = Vec::new();
        let report = drive_checkpointed(
            0i64,
            Tally::default(),
            "test walk",
            async |cursor: &i64| {
                Ok(match *cursor {
                    0 => page(vec![row(1), row(2)]),
                    2 => page(vec![row(3)]),
                    _ => page(vec![]),
                })
            },
            async |change: &SyncChange, report: &mut Tally, cursor: &mut i64| {
                report.rows += 1;
                *cursor = (*cursor).max(change.seq);
                Ok(())
            },
            async |cursor: &i64| {
                checkpoints.push(*cursor);
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(report.pages, 2);
        assert_eq!(report.rows, 3);
        assert_eq!(checkpoints, vec![2, 3]);
    }

    /// **Every accepted page ends in a breath** (`pass_breath` module docs):
    /// polled by hand over an instant feed — the only `Pending` a page loop
    /// with no network can return is the yield after a page's applies — a
    /// three-page walk returns `Pending` exactly three times, none for the
    /// empty page that ends it. Red-verified: without the breath the walk
    /// resolves on its first poll.
    #[tokio::test]
    async fn every_accepted_page_ends_in_a_breath() {
        let fut = drive_checkpointed(
            0i64,
            Tally::default(),
            "test walk",
            async |cursor: &i64| {
                Ok(match *cursor {
                    0 => page(vec![row(1)]),
                    1 => page(vec![row(2)]),
                    2 => page(vec![row(3)]),
                    _ => page(vec![]),
                })
            },
            async |change: &SyncChange, report: &mut Tally, cursor: &mut i64| {
                report.rows += 1;
                *cursor = (*cursor).max(change.seq);
                Ok(())
            },
            async |_: &i64| Ok(()),
        );
        let mut fut = std::pin::pin!(fut);
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut breaths = 0usize;
        let report = loop {
            match fut.as_mut().poll(&mut cx) {
                std::task::Poll::Ready(report) => break report.unwrap(),
                std::task::Poll::Pending => breaths += 1,
            }
        };
        assert_eq!(report.pages, 3);
        assert_eq!(
            breaths, 3,
            "one breath per accepted page, none for the empty page"
        );
    }

    /// A row naming a writer the frontier has never seen — the shape all
    /// three real frontier applies produce, and the one the spin refusal
    /// cannot see, because inserting a key *is* inequality. Left unbounded
    /// this page loop never returns: the counterpart spends 32 random bytes
    /// per row, the walk pays memory, a bigger frontier in every subsequent
    /// request, and (on the custodian's pull) a bigger durable blob per page.
    #[tokio::test]
    async fn a_frontier_fed_fresh_writers_forever_refuses_at_the_ceiling() {
        // One page per fresh writer, exactly as `custody_pull`'s cheapest
        // attack row does: coordinates only, no entry bytes.
        let err = drive(
            BTreeMap::new(),
            Tally::default(),
            "custody pull (__prefs)",
            async |cursor: &BTreeMap<String, i64>| Ok(page(vec![row(cursor.len() as i64)])),
            // The real applies' opening lines: insert the row's writer, raise
            // its slot. Nothing here validates that the writer exists — a
            // key-less custodian cannot.
            async |change: &SyncChange, report: &mut Tally, cursor: &mut BTreeMap<String, i64>| {
                report.rows += 1;
                let slot = cursor.entry(format!("writer-{}", change.seq)).or_insert(0);
                *slot = (*slot).max(change.seq);
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "custody pull (__prefs): the frontier names {} writers, \
                 past the {MAX_FRONTIER_WRITERS} a scope can have",
                MAX_FRONTIER_WRITERS + 1,
            ),
        );
    }

    /// The pair below pins the ceiling's VALUE as a literal, not read back
    /// off `MAX_FRONTIER_WRITERS` — unlike the test above, whose fixture and
    /// expectation share that constant and so follow it anywhere it moves. A frontier of exactly 4096 writers
    /// converges; one more refuses naming exactly 4097/4096. Mutating the
    /// constant in either direction reds one half of the pair: lower it and
    /// this test's 4096-writer frontier overgrows; raise it and the next
    /// test's 4097-writer frontier no longer does.
    #[tokio::test]
    async fn a_frontier_of_exactly_4096_writers_converges() {
        let report = drive(
            BTreeMap::new(),
            Tally::default(),
            "group-state walk",
            async |cursor: &BTreeMap<String, i64>| {
                Ok(if cursor.is_empty() {
                    page((0..4096i64).map(row).collect())
                } else {
                    page(vec![])
                })
            },
            async |change: &SyncChange, report: &mut Tally, cursor: &mut BTreeMap<String, i64>| {
                report.rows += 1;
                cursor.insert(format!("writer-{}", change.seq), change.seq);
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(report.pages, 1);
    }

    /// Companion to the test above: one writer past the literal 4096
    /// boundary refuses, naming the literal 4097/4096 pair — not values read
    /// off `MAX_FRONTIER_WRITERS`.
    #[tokio::test]
    async fn a_frontier_of_4097_writers_refuses_at_the_literal_boundary() {
        let err = drive(
            BTreeMap::new(),
            Tally::default(),
            "group-state walk",
            async |cursor: &BTreeMap<String, i64>| {
                Ok(if cursor.is_empty() {
                    page((0..4097i64).map(row).collect())
                } else {
                    page(vec![])
                })
            },
            async |change: &SyncChange, report: &mut Tally, cursor: &mut BTreeMap<String, i64>| {
                report.rows += 1;
                cursor.insert(format!("writer-{}", change.seq), change.seq);
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "group-state walk: the frontier names 4097 writers, past the 4096 a scope can have",
        );
    }

    /// The ceiling is the frontier shape's alone: a single-writer cursor is
    /// one number a counterpart cannot grow, so the same never-ending feed is
    /// stopped by the page budget instead — which is why both bounds exist.
    #[tokio::test]
    async fn a_seq_cursor_fed_forever_refuses_at_the_page_budget() {
        let err = drive(
            0i64,
            Tally::default(),
            "content-scope walk (media)",
            async |cursor: &i64| Ok(page(vec![row(*cursor + 1)])),
            async |change: &SyncChange, report: &mut Tally, cursor: &mut i64| {
                report.rows += 1;
                *cursor = (*cursor).max(change.seq);
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "content-scope walk (media): refusing to page past the \
                 {MAX_PAGES_PER_WALK}-page budget — the feed advances but never converges",
            ),
        );
    }

    /// The pair below pins the budget's VALUE as a literal PAGE COUNT, not
    /// read back off `MAX_PAGES_PER_WALK`. The
    /// boundary is a page count, not a number in the refusal string (the
    /// message always names the constant, however it's set) — so the literal
    /// pin is: 65535 accepted pages converge one page under the budget, and
    /// the 65536th accepted page is the last one banked before the walk
    /// refuses to extend a 65537th. Lowering `MAX_PAGES_PER_WALK` reds this
    /// test (the walk refuses before reaching 65535 pages); raising it reds
    /// the next test (more than 65536 pages get checkpointed before refusal).
    #[tokio::test]
    async fn a_walk_of_65535_pages_converges_one_page_under_the_budget() {
        let report = drive(
            0i64,
            Tally::default(),
            "content-scope walk (media)",
            async |cursor: &i64| {
                Ok(if *cursor < 65_535 {
                    page(vec![row(*cursor + 1)])
                } else {
                    page(vec![])
                })
            },
            async |change: &SyncChange, report: &mut Tally, cursor: &mut i64| {
                report.rows += 1;
                *cursor = (*cursor).max(change.seq);
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(report.pages, 65_535);
    }

    /// Companion to the test above: a feed that never stops advancing gets
    /// exactly 65536 pages checkpointed (banked as honest progress) before
    /// the walk refuses to fetch a 65537th — a literal page count, not one
    /// read off `MAX_PAGES_PER_WALK`.
    #[tokio::test]
    async fn the_walk_banks_exactly_65536_pages_before_refusing_a_65537th() {
        let mut checkpoints = 0usize;
        let err = drive_checkpointed(
            0i64,
            Tally::default(),
            "content-scope walk (media)",
            async |cursor: &i64| Ok(page(vec![row(*cursor + 1)])),
            async |change: &SyncChange, report: &mut Tally, cursor: &mut i64| {
                report.rows += 1;
                *cursor = (*cursor).max(change.seq);
                Ok(())
            },
            async |_: &i64| {
                checkpoints += 1;
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("page budget"), "{err}");
        assert_eq!(
            checkpoints, 65_536,
            "the walk must bank exactly 65536 pages before refusing to extend a 65537th",
        );
    }

    /// The budget counts pages, so it bounds the walk however the counterpart
    /// splits its rows — and it bounds the frontier shape too, for the feed
    /// that advances a writer it has already named (which grows no key, so the
    /// ceiling never fires).
    #[tokio::test]
    async fn a_frontier_advancing_one_known_writer_forever_refuses_at_the_budget() {
        let err = drive(
            BTreeMap::from([("aa".to_string(), 0i64)]),
            Tally::default(),
            "account-state walk",
            async |cursor: &BTreeMap<String, i64>| Ok(page(vec![row(cursor["aa"] + 1)])),
            async |change: &SyncChange, report: &mut Tally, cursor: &mut BTreeMap<String, i64>| {
                report.rows += 1;
                let slot = cursor.entry("aa".to_string()).or_insert(0);
                *slot = (*slot).max(change.seq);
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("page budget"),
            "the frontier never grew a key, so only the budget can stop this: {err}",
        );
    }

    /// A frontier that legitimately names many writers — a shared-set scope
    /// with a long succession history — walks to convergence untouched. The
    /// ceiling is a ceiling, not a working limit.
    #[tokio::test]
    async fn a_large_but_honest_frontier_converges() {
        let start: BTreeMap<String, i64> = (0..MAX_FRONTIER_WRITERS)
            .map(|i| (format!("w{i}"), 0))
            .collect();
        let report = drive(
            start,
            Tally::default(),
            "group-state walk",
            async |cursor: &BTreeMap<String, i64>| {
                Ok(match cursor["w0"] {
                    0 => page(vec![row(1)]),
                    _ => page(vec![]),
                })
            },
            async |change: &SyncChange, report: &mut Tally, cursor: &mut BTreeMap<String, i64>| {
                report.rows += 1;
                let slot = cursor.entry("w0".to_string()).or_insert(0);
                *slot = (*slot).max(change.seq);
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(report.pages, 1);
    }

    /// The overgrown frontier is never checkpointed — the durable cursor stays
    /// at the last page under the ceiling, so it stays sendable and the pull
    /// resumes from it rather than needing a meta row deleted by hand. The
    /// page budget's refusal is the deliberate opposite: it banks its page.
    #[tokio::test]
    async fn an_overgrown_frontier_is_never_checkpointed() {
        let mut checkpoints = 0usize;
        let err = drive_checkpointed(
            BTreeMap::new(),
            Tally::default(),
            "custody pull (__prefs)",
            // One page carries the whole overgrowth, so the walk crosses the
            // ceiling on its FIRST page and no page under it is ever accepted:
            // zero checkpoints is then unambiguous.
            async |_: &BTreeMap<String, i64>| {
                Ok(page(
                    (0..=MAX_FRONTIER_WRITERS as i64)
                        .map(row)
                        .collect::<Vec<_>>(),
                ))
            },
            async |change: &SyncChange, report: &mut Tally, cursor: &mut BTreeMap<String, i64>| {
                report.rows += 1;
                cursor.insert(format!("writer-{}", change.seq), change.seq);
                Ok(())
            },
            async |_: &BTreeMap<String, i64>| {
                checkpoints += 1;
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("past the"), "{err}");
        assert_eq!(
            checkpoints, 0,
            "the unsendable frontier must not be persisted",
        );
    }

    /// A stalled page is not checkpointed: the hook runs only past the spin
    /// refusal, so a crashed pull never resumes from a cursor no row moved.
    #[tokio::test]
    async fn a_stalled_page_is_never_checkpointed() {
        let mut checkpoints: Vec<i64> = Vec::new();
        let err = drive_checkpointed(
            9i64,
            Tally::default(),
            "custody pull (state)",
            async |_: &i64| Ok(page(vec![row(1)])),
            async |_: &SyncChange, report: &mut Tally, _: &mut i64| {
                report.rows += 1;
                Ok(())
            },
            async |cursor: &i64| {
                checkpoints.push(*cursor);
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("advanced the cursor past 9"));
        assert!(checkpoints.is_empty());
    }

    // ── The serve-order watermark ────────────────────────────────────────────
    //
    // Owner: `account-sync-plane.md` § Feeds and cursors → *Compaction is a
    // serve-order watermark*. These pin the driver half — where the echo is
    // taken, and what that does to the refusals and the checkpoint.

    fn echoed(rows: Vec<SyncChange>, complete_through_seq: Option<i64>) -> SyncChangesListReply {
        SyncChangesListReply {
            changes: rows,
            complete_through_seq,
            ..Default::default()
        }
    }

    /// The fix, at the driver: ONE page naming more writers than the ceiling,
    /// echoed, is projected before the refusals see it — nothing refuses, and
    /// the checkpoint banks a cursor naming nobody but carrying the watermark.
    #[tokio::test]
    async fn an_echoed_page_is_projected_before_the_ceiling_is_checked() {
        let tip = MAX_FRONTIER_WRITERS as i64 + 1;
        let mut banked: Vec<WatermarkCursor> = Vec::new();
        let report = drive_checkpointed(
            WatermarkCursor::open(None, None, BTreeMap::new(), BTreeMap::new()),
            Tally::default(),
            "account-state walk",
            async |cursor: &WatermarkCursor| {
                Ok(match cursor.held_through_seq() {
                    None => echoed((1..=tip).map(row).collect(), Some(tip)),
                    Some(held) => echoed(vec![], Some(held)),
                })
            },
            async |change: &SyncChange, report: &mut Tally, cursor: &mut WatermarkCursor| {
                report.rows += 1;
                cursor.saw(format!("writer-{}", change.seq), change.seq);
                Ok(())
            },
            async |cursor: &WatermarkCursor| {
                banked.push(cursor.clone());
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(report.pages, 1);
        assert_eq!(
            banked.len(),
            1,
            "the converging empty page echoed nothing new, so only the page was banked"
        );
        assert_eq!(banked[0].held_through_seq(), Some(tip));
        assert!(banked[0].frontier().is_empty());
    }

    /// Without an echo the same page refuses exactly as the whole frontier
    /// does — a walk never echoed (the peer leg) keeps the plain contract to the letter.
    #[tokio::test]
    async fn without_an_echo_the_watermark_cursor_refuses_at_the_ceiling() {
        let err = drive(
            WatermarkCursor::open(None, None, BTreeMap::new(), BTreeMap::new()),
            Tally::default(),
            "account-state walk",
            async |_: &WatermarkCursor| {
                Ok(echoed(
                    (0..=MAX_FRONTIER_WRITERS as i64).map(row).collect(),
                    None,
                ))
            },
            async |change: &SyncChange, report: &mut Tally, cursor: &mut WatermarkCursor| {
                report.rows += 1;
                cursor.saw(format!("writer-{}", change.seq), change.seq);
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "account-state walk: the frontier names {} writers, \
                 past the {MAX_FRONTIER_WRITERS} a scope can have",
                MAX_FRONTIER_WRITERS + 1,
            ),
        );
    }

    /// The spin refusal still bites: fresh writers the echo projects straight
    /// back out, under an echo that did not move, leave the next request
    /// exactly what it was.
    #[tokio::test]
    async fn an_echo_that_moves_nothing_is_still_a_stall() {
        let err = drive(
            WatermarkCursor::open(Some(5), None, BTreeMap::new(), BTreeMap::new()),
            Tally::default(),
            "account-state walk",
            async |_: &WatermarkCursor| Ok(echoed(vec![row(3)], Some(5))),
            async |change: &SyncChange, report: &mut Tally, cursor: &mut WatermarkCursor| {
                report.rows += 1;
                cursor.saw(format!("writer-{}", change.seq), change.seq);
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "account-state walk: a page of 1 rows advanced no writer cursor and no watermark past 5",
        );
    }

    /// A converged walk banks the whole log: an empty page whose echo raises
    /// the watermark is checkpointed, one whose echo moved nothing is not —
    /// and one that echoed NOTHING at all clears the unconfirmed bank `open`
    /// inherited, which moves the cursor (`Some(4)` → `None`) just as
    /// surely as a raise does.
    #[tokio::test]
    async fn an_empty_page_is_checkpointed_only_when_its_echo_moved_the_watermark() {
        for (echo, want) in [
            (Some(9), vec![Some(9)]),
            (Some(4), vec![]),
            (None, vec![None]),
        ] {
            let mut banked: Vec<Option<i64>> = Vec::new();
            drive_checkpointed(
                WatermarkCursor::open(Some(4), None, BTreeMap::new(), BTreeMap::new()),
                Tally::default(),
                "account-state walk",
                async |_: &WatermarkCursor| Ok(echoed(vec![], echo)),
                async |_: &SyncChange, _: &mut Tally, _: &mut WatermarkCursor| Ok(()),
                async |cursor: &WatermarkCursor| {
                    banked.push(cursor.held_through_seq());
                    Ok(())
                },
            )
            .await
            .unwrap();
            assert_eq!(banked, want, "empty page echoing {echo:?}");
        }
    }

    /// A banked watermark opens on the pinned writers alone; every projection
    /// keeps them at the slot their rows raised them to; and once this walk
    /// has confirmed the bank, no echo — lower, or negative — ever lowers it.
    #[test]
    fn a_projection_keeps_pinned_writers_and_never_lowers_the_watermark() {
        let mut cursor = WatermarkCursor::open(
            Some(10),
            None,
            BTreeMap::from([("kept".to_string(), 3), ("covered".to_string(), 8)]),
            BTreeMap::from([("kept".to_string(), 3)]),
        );
        assert_eq!(
            cursor.frontier(),
            &BTreeMap::from([("kept".to_string(), 3)])
        );

        cursor.saw("kept".into(), 5);
        cursor.saw("fresh".into(), 1);
        cursor.absorb_reply(&echoed(vec![], Some(10)));
        cursor.absorb_reply(&echoed(vec![], Some(7)));
        cursor.absorb_reply(&echoed(vec![], Some(-4)));

        assert_eq!(cursor.held_through_seq(), Some(10));
        assert_eq!(
            cursor.frontier(),
            &BTreeMap::from([("kept".to_string(), 5)])
        );
    }

    fn from_replica(
        complete_through_seq: Option<i64>,
        replica: Option<[u8; 16]>,
    ) -> SyncChangesListReply {
        SyncChangesListReply {
            complete_through_seq,
            replica_id: replica.map(|id| serde_bytes::ByteBuf::from(id.to_vec())),
            ..Default::default()
        }
    }

    /// A watermark is valid only for the replica it was banked from
    /// (`account-sync-plane.md` § The bind leg, ruling 2). The opening reply
    /// of a walk that inherited a bank against replica X voids it — the whole
    /// frontier restored, nothing banked from that reply — when it names
    /// replica Y, when it names none, and when it echoes below the bank; the
    /// same replica echoing at or above it confirms it.
    #[test]
    fn an_inherited_watermark_is_void_on_another_replica_none_or_a_lower_echo() {
        const X: [u8; 16] = [0xAA; 16];
        const Y: [u8; 16] = [0xBB; 16];
        let whole = BTreeMap::from([("w".to_string(), 3)]);
        for (reply, void) in [
            (from_replica(Some(12), Some(Y)), true),
            (from_replica(Some(12), None), true),
            (from_replica(Some(4), Some(X)), true),
            (from_replica(Some(10), Some(X)), false),
            (from_replica(Some(12), Some(X)), false),
        ] {
            let mut cursor =
                WatermarkCursor::open(Some(10), Some(X.to_vec()), whole.clone(), BTreeMap::new());
            assert!(cursor.frontier().is_empty(), "opens narrow on the bank");
            cursor.absorb_reply(&reply);
            let why = format!("{reply:?}");
            assert_eq!(cursor.watermark_dishonoured(), void, "{why}");
            if void {
                assert_eq!(cursor.held_through_seq(), None, "{why}");
                assert_eq!(cursor.replica_id(), None, "{why}");
                assert_eq!(cursor.frontier(), &whole, "{why}");
            } else {
                assert_eq!(
                    cursor.held_through_seq(),
                    reply.complete_through_seq,
                    "{why}"
                );
                assert_eq!(cursor.replica_id(), Some(X.as_slice()), "{why}");
            }
        }
    }

    /// After a void, the next echo banks afresh — keyed to the replica that
    /// sent it; and a reply from another replica voids even a bank this walk
    /// already confirmed.
    #[test]
    fn a_void_bank_rebanks_under_the_replica_that_echoes_next() {
        const X: [u8; 16] = [0xAA; 16];
        const Y: [u8; 16] = [0xBB; 16];
        let mut cursor =
            WatermarkCursor::open(Some(40), Some(X.to_vec()), BTreeMap::new(), BTreeMap::new());
        cursor.absorb_reply(&from_replica(Some(7), Some(Y)));
        assert_eq!(cursor.held_through_seq(), None);
        cursor.absorb_reply(&from_replica(Some(7), Some(Y)));
        assert_eq!(cursor.held_through_seq(), Some(7));
        assert_eq!(cursor.replica_id(), Some(Y.as_slice()));

        cursor.absorb_reply(&from_replica(Some(9), Some(X)));
        assert_eq!(
            cursor.held_through_seq(),
            None,
            "a confirmed bank is void on another replica"
        );
    }

    /// With no watermark yet, nothing is projected: the cursor opens on the
    /// whole frontier, a pinned writer the stored frontier lacks folded in at
    /// its slot.
    #[test]
    fn with_no_watermark_the_cursor_opens_on_the_whole_frontier() {
        let cursor = WatermarkCursor::open(
            None,
            None,
            BTreeMap::from([("accounted".to_string(), 2)]),
            BTreeMap::from([("unaccounted".to_string(), 0)]),
        );
        assert_eq!(cursor.held_through_seq(), None);
        assert_eq!(
            cursor.frontier(),
            &BTreeMap::from([("accounted".to_string(), 2), ("unaccounted".to_string(), 0)])
        );
    }
}
