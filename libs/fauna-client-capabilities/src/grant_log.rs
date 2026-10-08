//! Now/History fold over the owner's signed capability-grant event log
//! (`fauna.state.succession-ledger`'s `event/…` rows, read as the
//! [`SuccessionLedger`] fold) — the shared-Rust half of the Nests-page
//! trust facet (part of the capability-mediated content-processing design,
//! tracked internally — the "forensic grant log" grant-event-log slice).
//! "Now" folds the log to each grant's current state (the latest event per
//! grant, omitted if that event is a revoke); "History" is the raw
//! timeline — two views of one event-sourced log ("Now = projection,
//! History = the log").
//!
//! Every [`fauna_core::grant_event::GrantEvent`] is self-describing (it
//! carries the grant's full scope/window as of that event, not just a
//! delta), so both folds are simple latest-event lookups with no need to
//! correlate a `Renew`/`Revoke` event back against an earlier `Mint` for
//! context. [`record_renew`] is what upholds that: it carries the existing
//! scope forward from the current state rather than taking a fresh one from
//! the caller (renew, per the design's `RenewGrantRequest`, only ever
//! extends the window — scope is fixed at mint).
//!
//! The recording helpers mirror `fauna_client_subscriptions::custody`'s pure
//! transition style: given an in-memory [`SuccessionLedger`] + the minting
//! client's identity signing key, they append a signed event and leave
//! persisting it to the caller — a merge of the new events through the
//! `fauna_client_config::SuccessionLedgerStore` seam
//! ([`SuccessionLedger::events_replica`]), whose door re-verifies every event
//! against the attested signer set.

use ed25519_dalek::SigningKey;
use fauna_client_config::PublishedLedger;
use fauna_core::grant_event::{
    GRANT_SCOPE_TIER_BOUNDED, GrantEvent, GrantEventError, GrantEventKind, GrantEventScope,
};
use fauna_core::succession_ledger::SuccessionLedger;
use fauna_mls::wrapped_blob::ScopeTuple;
use fauna_protocol::wrapped_blob::MAX_GRANTS_PER_OWNER;

/// A [`record_renew`] call named a `grant_id` with no current (non-revoked)
/// grant to renew, or asked the window to slide backward.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenewError {
    /// No `Mint`/`Renew` event for this `grant_id` is the log's latest for
    /// it — either it was never minted, or it has already been revoked.
    #[error("no current grant for this grant_id (never minted, or already revoked)")]
    GrantNotFound,
    /// The renewal's `window_start` is earlier than the grant's current one.
    /// A renewal only slides the window forward ([`renewal_window`]): the
    /// nest refuses a backward move too, since the wraps for the epochs it
    /// would re-cover are gone.
    #[error("a renewal cannot move a grant's window start backward")]
    WindowStartMovedBack,
    /// The signature step failed.
    #[error(transparent)]
    Sign(#[from] GrantEventError),
}

/// Build an **unsigned** grant event (placeholder `sig`). Pure — no key, no
/// `ledger` mutation. This is the split point that lets the machine wiring
/// (`fauna-client-pair`) sign through a `GrantEventSigner` seam so the raw
/// identity `SigningKey` never crosses the UniFFI/WASM boundary: the seam
/// consumes one of the public `build_*_event` outputs, signs it, and the
/// caller commits it with [`append_signed`]. The direct-key convenience path
/// (`record_*`, for tests + non-FFI callers) signs via [`sign_and_append`].
#[allow(clippy::too_many_arguments)]
fn build_event(
    grant_id: [u8; 16],
    holder: [u8; 32],
    kind: GrantEventKind,
    scope: Vec<GrantEventScope>,
    window_start: u64,
    window_end: u64,
    at: u64,
) -> GrantEvent {
    GrantEvent {
        grant_id: grant_id.to_vec(),
        holder: holder.to_vec(),
        kind,
        scope,
        window_start,
        window_end,
        at,
        sig: vec![0u8; fauna_core::grant_event::GRANT_EVENT_SIGNATURE_LEN],
    }
}

/// Sign `event` with `signing_key` (the direct-key path) and append it to the
/// log; the caller persists the new event.
fn sign_and_append(
    ledger: &mut SuccessionLedger,
    signing_key: &SigningKey,
    event: GrantEvent,
) -> Result<GrantEvent, GrantEventError> {
    let signed = event.sign(signing_key)?;
    ledger.grant_events.push(signed.clone());
    Ok(signed)
}

/// Append an already-signed [`GrantEvent`] — the seam-path twin of the push
/// inside `record_*`, for the machine wiring after
/// `GrantEventSigner::sign_grant_event`. Callers persist the event. (The event's
/// signature is the minting client's own; the succession ledger's merge dedups
/// by full equality, so a redundant append is idempotent.)
pub fn append_signed(ledger: &mut SuccessionLedger, event: GrantEvent) {
    ledger.grant_events.push(event);
}

/// Render a grant id for an error message.
fn grant_id_hex(id: &[u8; 16]) -> String {
    fauna_core::format::hex_full(id)
}

/// A sealed `GrantBlob` that is **not yet safe to put on the nest**, and the
/// type that keeps it that way.
///
/// **The ordering rule this type carries.** The nest deposit
/// (`fauna.capabilities.mint`) is the moment a capability becomes LIVE; the
/// owner's signed `Mint` event is the only thing that makes it *visible and
/// revocable*. The Nests page projects the **client log**, not the nest;
/// `fauna.capabilities.revoke` needs a `grant_id` only the log carries; and the
/// wire has **no owner-side enumerate** (`mint` / holder-authenticated `fetch` /
/// `renew` / `revoke` plus the scoring worklists — nothing lists *my* grants).
/// So a grant the nest holds and the log does not is unreachable **today**: on
/// no page, nameable by no revoke call, discoverable by nothing. (The ratified
/// heal for already-stranded rows is the ids-only reconcile sweep —
/// `ui/nests.md` § Trust facet — grants → *Reconcile*, ruled 2026-08-15, not
/// yet built; it makes this ordering the primary defense rather than the only
/// one, never a license to relax it.)
///
/// Therefore the log must be durable **before** the blob ships — and not only
/// on this device: a sibling replica learns of the `Mint` only through the
/// bound nest's state plane, and its reconcile sweep revokes a row whose event
/// it cannot read, so the event must be **acknowledged by the nest** before the
/// deposit (the published form, ruled 2026-10-06 — `ui/nests.md` § Trust facet
/// — grants → *Record-then-deposit*). Inverted, the
/// same interruption costs only a *phantom row* — a log entry with no nest
/// blob, which is visible, and revoking it is idempotent nest-side
/// (`bridge_blob_handlers.rs`: "revoking an absent grant still replies
/// `{ ok: true }`"), so the user can clear it. Over-reporting a capability is
/// recoverable; under-reporting one is not. That asymmetry is the whole rule.
///
/// **Why a type and not a doc comment.** Until 2026-08-14 every minting site ran
/// the unsafe order, and one had copied it from another *citing it as canonical
/// prior art* (`fauna-client-mail-settings`' `mint_baseline_grant`: "The nest
/// deposit runs first, then the log records the mint (mirrors
/// `LinkedNestsMachine::mint`)"). A comment propagated the bug; a type cannot.
/// [`Self::release`] is the only way to reach the bytes, and it demands
/// [`PublishedGrants`], built only from the ledger the seam's
/// `merge_published` stored and the nest acknowledged.
///
/// Invariant owner: `docs/goal/principles.md` § The user always controls their
/// data (grants are "revocable, audited from the user's app") +
/// `docs/goal/architecture/nest/common.md` § Client-state recoverability.
#[must_use = "an UndepositedGrant must be released against PublishedGrants and deposited — \
              dropping it silently skips the mint"]
pub struct UndepositedGrant {
    grant_id: [u8; 16],
    blob_bytes: Vec<u8>,
}

impl UndepositedGrant {
    /// Hold `blob_bytes` (the canonical `GrantBlob` encoding) until its `Mint`
    /// event is durable and acknowledged by the bound nest. `grant_id` must be the id inside that blob — it is what
    /// [`Self::release`] looks for in the stored log.
    pub fn new(grant_id: [u8; 16], blob_bytes: Vec<u8>) -> Self {
        Self {
            grant_id,
            blob_bytes,
        }
    }

    /// The grant this blob mints.
    pub fn grant_id(&self) -> [u8; 16] {
        self.grant_id
    }

    /// Release the blob for deposit — **only** against a nest-acknowledged log
    /// that already records its `Mint`.
    ///
    /// This is a real check, not a ceremony: [`PublishedGrants`] is read from
    /// the ledger `merge_published` *returned*, so a write that dropped our
    /// event (a READ fold that does not admit its signer) is caught here, and a
    /// publish the nest did not acknowledge never produced the proof at all —
    /// either way, rather than producing exactly the orphan this type exists to
    /// prevent.
    pub fn release(self, published: &PublishedGrants) -> Result<Vec<u8>, UnrecordedGrantError> {
        if published.minted.contains(&self.grant_id) {
            Ok(self.blob_bytes)
        } else {
            Err(UnrecordedGrantError {
                grant_id: grant_id_hex(&self.grant_id),
            })
        }
    }
}

/// Proof that a set of `Mint` events reached durable storage **and the bound
/// nest's state plane**.
///
/// Built **only** from a [`PublishedLedger`] — the answer of
/// `SuccessionLedgerStore::merge_published`, which is the ledger actually
/// stored (post-merge, never the one the caller hoped to store) after every
/// ledger row this device wrote was acknowledged by the nest. A local
/// read-back alone (the pre-2026-10-06 `RecordedGrants`) proved the event
/// durable on one device and on no nest, so a sibling's reconcile sweep could
/// meet the deposited row before it could read the event.
pub struct PublishedGrants {
    minted: Vec<[u8; 16]>,
}

impl PublishedGrants {
    /// Collect every grant id the acknowledged log records a `Mint` for.
    ///
    /// A later `Revoke` does not remove the id: this answers "is this grant in
    /// the user's log, so the page can show it and `revoke` can name it", which
    /// a revoked grant satisfies (its row is in History, and re-revoking is
    /// idempotent). The question `latest_live_events` answers — "is it live
    /// *now*" — is a different one.
    pub fn from_published(published: &PublishedLedger) -> Self {
        Self {
            minted: published
                .ledger()
                .grant_events
                .iter()
                .filter(|e| e.kind == GrantEventKind::Mint)
                .filter_map(|e| <[u8; 16]>::try_from(e.grant_id.as_slice()).ok())
                .collect(),
        }
    }
}

/// [`UndepositedGrant::release`] was handed a [`PublishedGrants`] with no `Mint`
/// event for that grant — depositing anyway would strand a live capability the
/// user's app can neither show nor revoke.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "refusing to deposit capability grant {grant_id}: the published grant log records no Mint \
     event for it, so the user's app could neither show nor revoke it"
)]
pub struct UnrecordedGrantError {
    /// The grant id, hex-encoded.
    pub grant_id: String,
}

/// Build the **unsigned** `Mint` event for the machine wiring's signer-seam
/// path (`GrantEventSigner::sign_grant_event` → [`append_signed`]). The
/// direct-key twin is [`record_mint`].
pub fn build_mint_event(
    grant_id: [u8; 16],
    holder: [u8; 32],
    scope: Vec<GrantEventScope>,
    window_start: u64,
    window_end: u64,
    at: u64,
) -> GrantEvent {
    build_event(
        grant_id,
        holder,
        GrantEventKind::Mint,
        scope,
        window_start,
        window_end,
        at,
    )
}

/// Build the **unsigned** `Renew` event: looks up the grant's current
/// holder/scope (scope is fixed at mint) and carries them forward with the
/// slid window `[new_window_start, new_window_end]` — the pair
/// [`renewal_window`] computes for a renewal, or the recorded window itself
/// for the rotation heal's equal-window key refresh. The signer-seam twin of
/// [`record_renew`].
///
/// # Errors
///
/// [`RenewError::GrantNotFound`] if `grant_id` has no current (non-revoked)
/// grant to renew; [`RenewError::WindowStartMovedBack`] if `new_window_start`
/// is earlier than the grant's current start.
pub fn build_renew_event(
    ledger: &SuccessionLedger,
    grant_id: &[u8; 16],
    new_window_start: u64,
    new_window_end: u64,
    at: u64,
) -> Result<GrantEvent, RenewError> {
    let current = current_grants(ledger)
        .into_iter()
        .find(|g| g.grant_id == grant_id.as_slice())
        .ok_or(RenewError::GrantNotFound)?;
    if new_window_start < current.window_start {
        return Err(RenewError::WindowStartMovedBack);
    }
    let mut holder = [0u8; 32];
    holder.copy_from_slice(&current.holder);
    Ok(build_event(
        *grant_id,
        holder,
        GrantEventKind::Renew,
        current.scope,
        new_window_start,
        new_window_end,
        at,
    ))
}

/// The window a renewal of `current` at `now` records and sends: the end
/// moves to `now + extend_by_secs` (the grant's own mint-time length,
/// [`renewal_window_secs`]) and the start **re-centres** to `now -
/// extend_by_secs`, never behind the current start — so a renewed grant keeps
/// one mint-length of history behind the renewal instant and one ahead, and
/// its window is never wider than twice its mint-time length. The retention
/// ruling (`encryption-at-rest.md` § Capability tiering → *Content-sealing
/// epochs*): a bounded mail grant's per-epoch wraps follow the window, so a
/// window that only grew would carry one more wrap per week for ever and
/// reach the nest's size cap within months; the nest prunes the wraps below
/// the slid start in the same renew. The manual renew and the auto-renew
/// loop both use it, so the two agree with the rotation heal (which re-wraps
/// the recorded window as it stands) on the retained epoch set. Uniform
/// across regimes: a master-key grant's start slides the same way (its
/// holder honors the window against the clock, so nothing changes for it).
#[must_use]
pub fn renewal_window(current: &CurrentGrant, extend_by_secs: u64, now: u64) -> (u64, u64) {
    let end = now.saturating_add(extend_by_secs);
    let start = current.window_start.max(now.saturating_sub(extend_by_secs));
    (start, end)
}

/// Build the **unsigned** `Revoke` event (keyless — `scope` empty,
/// window zeroed). The signer-seam twin of [`record_revoke`].
pub fn build_revoke_event(grant_id: [u8; 16], holder: [u8; 32], at: u64) -> GrantEvent {
    build_event(grant_id, holder, GrantEventKind::Revoke, vec![], 0, 0, at)
}

/// Record a grant's creation (`fauna.capabilities.mint`). Appends a signed
/// `Mint` event to `ledger.grant_events` and returns it; the caller
/// persists `ledger` (same pattern as `custody::record_new_tier`).
#[allow(clippy::too_many_arguments)]
pub fn record_mint(
    ledger: &mut SuccessionLedger,
    signing_key: &SigningKey,
    grant_id: [u8; 16],
    holder: [u8; 32],
    scope: Vec<GrantEventScope>,
    window_start: u64,
    window_end: u64,
    at: u64,
) -> Result<GrantEvent, GrantEventError> {
    sign_and_append(
        ledger,
        signing_key,
        build_mint_event(grant_id, holder, scope, window_start, window_end, at),
    )
}

/// Record a grant's renewal (`fauna.capabilities.renew`). A renewal slides
/// the window ([`renewal_window`]) and never touches the scope (the
/// `RenewGrantRequest` carries no scope field — scope is fixed at mint), so
/// this looks up the grant's current holder/scope via [`current_grants`] and
/// carries them forward into the new event with the slid window.
///
/// # Errors
///
/// Returns [`RenewError::GrantNotFound`] if `grant_id` has no current
/// (non-revoked) grant to renew, [`RenewError::WindowStartMovedBack`] if the
/// start would move backward.
pub fn record_renew(
    ledger: &mut SuccessionLedger,
    signing_key: &SigningKey,
    grant_id: &[u8; 16],
    new_window_start: u64,
    new_window_end: u64,
    at: u64,
) -> Result<GrantEvent, RenewError> {
    let event = build_renew_event(ledger, grant_id, new_window_start, new_window_end, at)?;
    Ok(sign_and_append(ledger, signing_key, event)?)
}

/// Record a grant's revocation (`fauna.capabilities.revoke`). Appends a
/// signed `Revoke` event — keyless per [`GrantEvent`]'s doc (`scope` empty,
/// `window_start`/`window_end` zeroed; the prior events already recorded
/// what was granted, which the History view still shows).
pub fn record_revoke(
    ledger: &mut SuccessionLedger,
    signing_key: &SigningKey,
    grant_id: [u8; 16],
    holder: [u8; 32],
    at: u64,
) -> Result<GrantEvent, GrantEventError> {
    sign_and_append(
        ledger,
        signing_key,
        build_revoke_event(grant_id, holder, at),
    )
}

// The folded "Now" state and its selection moved to `fauna-core::grant_event`
// (pure logic over the ledger's own log, consumed below this crate by the
// succession aftermath's mark write); re-exported here so every existing
// consumer keeps resolving.
pub use fauna_core::grant_event::{CurrentGrant, current_grants, latest_live_events};

/// The window length the user chose when `grant_id` was minted — its `Mint`
/// event's `window_end - window_start`, which is the only place the chosen
/// duration is recorded (`nests.md` § Data shape). A `Renew` slides the
/// window ([`renewal_window`]), so the current window's width is not the
/// chosen length; this reads the `Mint` itself. `None` when the log holds no
/// `Mint` for the id (the earliest one wins if a merge somehow carries two).
pub fn minted_window_secs(ledger: &SuccessionLedger, grant_id: &[u8]) -> Option<u64> {
    ledger
        .grant_events
        .iter()
        .filter(|e| e.kind == GrantEventKind::Mint && e.grant_id == grant_id)
        .min_by_key(|e| e.at)
        .map(|e| e.window_end.saturating_sub(e.window_start))
}

/// How far a renewal of `grant_id` extends it: the grant's own mint-time
/// length ([`minted_window_secs`]), falling back to
/// [`crate::DEFAULT_GRANT_WINDOW_SECS`] for an id whose `Mint` the log does
/// not hold. Manual renew and the auto-renew loop both use it, so renewing a
/// grant never changes the duration the user picked.
pub fn renewal_window_secs(ledger: &SuccessionLedger, grant_id: &[u8]) -> u64 {
    minted_window_secs(ledger, grant_id).unwrap_or(crate::DEFAULT_GRANT_WINDOW_SECS)
}

/// The reconcile sweep's predicate: of the `grant_id`s a nest reports holding
/// for this owner, which does the log **not** hold live?
///
/// Lives here beside [`latest_live_events`] because that fold *is* the
/// judgement — an id is recognized iff it survives the fold (no `Revoke` in its
/// history, latest event `Mint`/`Renew`). Everything else — an id with no event
/// at all (a deposit whose `Mint` never became durable, or a row a hostile nest
/// invented) and an id whose log carries a `Revoke` (a row resurrected after
/// revocation) — is unrecognized, and the sweep revokes it on the answering
/// nest.
///
/// Three properties are load-bearing (`ui/nests.md` § Trust facet — grants →
/// *Reconcile*), and each is a property of this function rather than of its
/// caller so that no call site can opt out:
///
/// 1. **Nothing from `nest_ids` enters client state.** This reads `ledger`; it
///    never writes it. A `Revoke` event for an id the log never minted would be
///    nest-influenced content entering the signed log — the precise channel the
///    audit rule exists to close — so the sweep appends no [`GrantEvent`] at
///    all, and that is enforced by this taking `&SuccessionLedger`.
/// 2. **Bounded by the per-owner cap.** An honest nest's reply is
///    ≤ [`MAX_GRANTS_PER_OWNER`] ids by construction, so a longer answer is
///    hostile and is truncated here rather than fanned out into an unbounded
///    revoke storm. Truncation is deliberately *before* the filter: the bound
///    is on what the client will process at all, not on what survives it.
/// 3. **Ids only, judged against the log alone.** The reply carries no scope,
///    holder or window (it cannot — [`ReconcileGrantsReply`] has no such
///    fields), so liveness is decided entirely client-side. An id whose latest
///    event is `Mint`/`Renew` is skipped by construction: renewal and expiry
///    judgements stay log-side, never nest-side.
///
/// Order within the result follows `nest_ids`; duplicates in a hostile answer
/// are preserved (revoke is idempotent, and de-duplicating would be a second,
/// unbounded allocation over attacker-chosen input for no gain).
///
/// [`MAX_GRANTS_PER_OWNER`]: fauna_protocol::wrapped_blob::MAX_GRANTS_PER_OWNER
/// [`ReconcileGrantsReply`]: fauna_protocol::wrapped_blob::ReconcileGrantsReply
#[must_use]
pub fn unrecognized_grant_ids(
    ledger: &SuccessionLedger,
    nest_ids: impl IntoIterator<Item = [u8; 16]>,
) -> Vec<[u8; 16]> {
    let live: std::collections::BTreeSet<&[u8]> = latest_live_events(ledger)
        .into_iter()
        .map(|e| e.grant_id.as_slice())
        .collect();
    nest_ids
        .into_iter()
        .take(MAX_GRANTS_PER_OWNER)
        .filter(|id| !live.contains(id.as_slice()))
        .collect()
}

/// The `content.read{mail}` [`GrantEventScope`] a **bounded** mail grant's
/// `Mint`/`Renew` events record — carrying the log-side regime marker
/// [`GRANT_SCOPE_TIER_BOUNDED`] in `tier`. Every bounded-mail mint event MUST
/// use this (required from day one — `mint_bounded_mail_grant` shipped with
/// zero production callers, so no unmarked legacy events exist); the marker is
/// what the rotation-heal driver enumerates by and the Nests-page regime copy
/// renders. Log-only: the `GrantBlob`'s own `ScopeTuple` stays `tier: None`.
#[must_use]
pub fn bounded_mail_event_scope() -> GrantEventScope {
    GrantEventScope {
        class: ScopeTuple::CLASS_CONTENT_READ.to_string(),
        kind: Some(ScopeTuple::KIND_MAIL.to_string()),
        tier: Some(GRANT_SCOPE_TIER_BOUNDED.to_string()),
    }
}

/// The event scope a **per-labeler** bounded mail grant's `Mint` records —
/// what subscribing a `wasm` mail labeler mints
/// (`content-moderation-and-ranking.md` § Tier-3 → *Subscribing = minting a
/// capability*; the blob is [`crate::mint_bounded_mail_labeler_grant`]). The
/// bounded `content.read{mail}` tuple plus the keyless `content.label-write`
/// tuple, **both** carrying the labeler's factor folded into `kind`
/// ([`GrantEventScope::with_factor`] — the log-side twin of the blob's
/// `ScopeTuple::factor`, one qualifier per tuple exactly as the wraps carry
/// it). [`labeler_factor_of_grant`] reads it back; the Nests page renders it
/// as *run labeler ‹id› over my mail* rather than a second anonymous mail
/// row, and unsubscribe finds the grant to revoke by it.
#[must_use]
pub fn bounded_mail_labeler_event_scope(
    labeler_id: &fauna_core::identity::ActorId,
) -> Vec<GrantEventScope> {
    let factor = fauna_core::scoring::labeler_factor(labeler_id);
    vec![
        bounded_mail_event_scope().with_factor(&factor),
        GrantEventScope {
            class: ScopeTuple::CLASS_CONTENT_LABEL_WRITE.to_string(),
            kind: None,
            tier: None,
        }
        .with_factor(&factor),
    ]
}

/// The event scope a blob's declared tuples record — each tuple's
/// `class`/`kind`/`tier` with its `factor` folded into `kind`
/// ([`GrantEventScope::with_factor`]), the inverse of the re-mint's
/// unfolding. What the consent-time `ext.*` grant's `Mint` logs: the
/// `content.write` tuples' writer factor is the replica-side admission's
/// whole input (`fauna_core::grant_event::content_write_authorizations`).
#[must_use]
pub fn event_scope_of(tuples: &[ScopeTuple]) -> Vec<GrantEventScope> {
    tuples
        .iter()
        .map(|t| {
            let scope = GrantEventScope {
                class: t.class.clone(),
                kind: t.kind.clone(),
                tier: t.tier.clone(),
            };
            match t.factor.as_deref() {
                Some(factor) => scope.with_factor(factor),
                None => scope,
            }
        })
        .collect()
}

/// The one `labeler:<hex>` factor a per-labeler grant's every tuple carries
/// ([`bounded_mail_labeler_event_scope`]), or `None` for a grant that carries
/// none — or whose tuples disagree, which no minting site produces and which
/// therefore reads as *not a per-labeler grant* rather than as any one
/// labeler's.
#[must_use]
pub fn labeler_factor_of_grant(scope: &[GrantEventScope]) -> Option<String> {
    let mut factors = scope.iter().filter_map(GrantEventScope::factor);
    let first = factors.next()?;
    if !fauna_core::scoring::is_labeler_factor(first) || factors.any(|f| f != first) {
        return None;
    }
    Some(first.to_string())
}

/// The live (non-revoked) grant that licenses `labeler_id` over the owner's
/// sealed content — the subscription's 1:1 twin, found by the factor its
/// tuples carry. Unsubscribe revokes this one. `None` when the subscription
/// was registered without a grant (no holder to mint to at the time).
#[must_use]
pub fn current_labeler_grant(
    ledger: &SuccessionLedger,
    labeler_id: &fauna_core::identity::ActorId,
) -> Option<CurrentGrant> {
    let factor = fauna_core::scoring::labeler_factor(labeler_id);
    current_grants(ledger)
        .into_iter()
        .find(|g| labeler_factor_of_grant(&g.scope).as_deref() == Some(factor.as_str()))
}

/// Whether a folded grant's declared scope marks it as a **bounded** mail
/// grant ([`bounded_mail_event_scope`] present, with or without a folded
/// labeler factor — a per-labeler grant is bounded too). A hint, not an
/// enforcer — the nest's regime-crossing renew guard arbitrates: a mismarked
/// master grant's heal-renew is refused nest-side, and an unmarked bounded
/// grant simply heals at its next renew instead of at rotation.
#[must_use]
pub fn is_bounded_mail_grant(scope: &[GrantEventScope]) -> bool {
    scope.iter().any(|s| {
        s.class == ScopeTuple::CLASS_CONTENT_READ
            && s.base_kind() == Some(ScopeTuple::KIND_MAIL)
            && s.tier.as_deref() == Some(GRANT_SCOPE_TIER_BOUNDED)
    })
}

/// The minting client's identity-key seam for the grant-event log: the impl
/// holds the raw Ed25519 `SigningKey` and signs a fully-populated
/// (placeholder-`sig`) [`GrantEvent`] in place, so the key never crosses a
/// machine's FFI/wasm boundary (`key-material-hierarchy.md` #7). One trait for
/// every minting machine (the Nests page's `LinkedNestsMachine`, the labeler
/// catalog's `LabelerCatalogMachine`); `fauna-client-mail-settings`' wider
/// `IdentitySigner` (which also signs submission tokens) implements it too.
/// Synchronous (no transport) — just `MaybeSendSync` so a wasm impl may hold a
/// `!Send` handle.
pub trait GrantEventSigner: fauna_core::MaybeSendSync {
    fn sign_grant_event(&self, event: GrantEvent) -> Result<GrantEvent, GrantEventSignError>;
}

/// A [`GrantEventSigner`] could not sign (the `Display` of the underlying
/// [`GrantEventError`], carried as text so the seam stays object-safe across
/// FFI-flattened error types).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrantEventSignError {
    #[error("grant-event sign: {0}")]
    Sign(String),
}

/// The [`GrantEventSigner`] every machine that holds the owner's identity
/// keypair builds: it signs a fully-populated (placeholder-`sig`) event in
/// place, so the raw key never crosses the machine's FFI/wasm boundary
/// (`key-material-hierarchy.md` #7). Transport-free, so one impl serves native
/// and wasm.
pub struct KeypairGrantEventSigner {
    keypair: fauna_core::identity::ActorKeypair,
}

impl KeypairGrantEventSigner {
    /// A signer over a copy of `keypair`.
    pub fn new(keypair: &fauna_core::identity::ActorKeypair) -> Self {
        Self {
            keypair: fauna_core::identity::ActorKeypair::from_secret(*keypair.secret_bytes()),
        }
    }
}

impl GrantEventSigner for KeypairGrantEventSigner {
    fn sign_grant_event(&self, event: GrantEvent) -> Result<GrantEvent, GrantEventSignError> {
        event
            .sign(self.keypair.signing_key())
            .map_err(|e| GrantEventSignError::Sign(e.to_string()))
    }
}

/// Why [`record_revokes`] recorded nothing.
#[derive(Debug, thiserror::Error)]
pub enum RecordRevokesError {
    #[error(transparent)]
    Sign(#[from] GrantEventSignError),
    #[error(transparent)]
    Store(#[from] fauna_client_config::StoreError),
}

/// Sign a `Revoke` event for each `(grant_id, holder)` in `ended` and join
/// them into the owner's log store in one write — the seam twin of
/// [`record_revoke`] for a machine holding a [`GrantEventSigner`] and a
/// `SuccessionLedgerStore` rather than a key and a ledger. Revoke narrows, so
/// the caller ends the grants on the nest first and records them here after.
/// Nothing to record writes nothing.
///
/// # Errors
/// The signer's or the store's refusal; either leaves the log unwritten.
pub async fn record_revokes(
    ledger: &dyn fauna_client_config::SuccessionLedgerStore,
    signer: &dyn GrantEventSigner,
    owner: [u8; 32],
    ended: &[([u8; 16], [u8; 32])],
    at: u64,
) -> Result<(), RecordRevokesError> {
    if ended.is_empty() {
        return Ok(());
    }
    let signed = ended
        .iter()
        .map(|(grant_id, holder)| {
            signer.sign_grant_event(build_revoke_event(*grant_id, *holder, at))
        })
        .collect::<Result<Vec<_>, _>>()?;
    ledger
        .merge(SuccessionLedger::events_replica(
            fauna_core::identity::ActorId(owner),
            signed,
        ))
        .await?;
    Ok(())
}

/// The `Revoke`s owed when `fauna.principals.revoke` ended a principal's
/// grants nest-side: one per grant the owner's log still holds live for the
/// principal's `holder` ([`crate::view_model::grants_ended_by_principal_revoke`]),
/// recorded so the history lens can say why they vanished. Returns how many
/// were recorded.
///
/// # Errors
/// The store's read, or [`record_revokes`]'s.
pub async fn record_principal_revoke(
    ledger: &dyn fauna_client_config::SuccessionLedgerStore,
    signer: &dyn GrantEventSigner,
    owner: [u8; 32],
    holder: [u8; 32],
    at: u64,
) -> Result<usize, RecordRevokesError> {
    let stored = ledger.load().await?;
    let ended: Vec<([u8; 16], [u8; 32])> =
        crate::view_model::grants_ended_by_principal_revoke(&stored, &holder)
            .into_iter()
            .filter_map(|g| Some((<[u8; 16]>::try_from(g.grant_id.as_slice()).ok()?, holder)))
            .collect();
    record_revokes(ledger, signer, owner, &ended, at).await?;
    Ok(ended.len())
}

/// Causal order rank within a single `at` second: a grant's lifecycle can only
/// go `Mint → Renew* → Revoke`, so when two of its events share a second (a
/// same-second transition, or a coarse clock) the higher rank is the more recent
/// one. Sorting on this before the `sig` tie-break keeps History causally correct
/// ("most-recent-first" shows the `Revoke` above its same-second `Mint`) and
/// deterministic regardless of the merge order.
fn causal_rank(kind: GrantEventKind) -> u8 {
    match kind {
        GrantEventKind::Mint => 0,
        GrantEventKind::Renew => 1,
        GrantEventKind::Revoke => 2,
    }
}

/// The full event timeline, most-recent-first — the Nests-page "History"
/// lens (event-sourced: this is the log itself, Now above is its
/// projection). Ordered by `at` desc, then [`causal_rank`] desc (so a
/// same-second `Revoke` sorts above its `Mint`), then `sig` bytes as the final
/// deterministic tie-break (the log is an unordered `Vec` after a cross-device
/// union merge).
pub fn history(ledger: &SuccessionLedger) -> Vec<&GrantEvent> {
    let mut events: Vec<&GrantEvent> = ledger.grant_events.iter().collect();
    events.sort_by(|a, b| {
        (b.at, causal_rank(b.kind), &b.sig).cmp(&(a.at, causal_rank(a.kind), &a.sig))
    });
    events
}

/// One grant's history, most-recent-first — the History view when the user
/// drills into a single grant.
pub fn history_for_grant<'a>(ledger: &'a SuccessionLedger, grant_id: &[u8]) -> Vec<&'a GrantEvent> {
    history(ledger)
        .into_iter()
        .filter(|e| e.grant_id == grant_id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::identity::{ActorId, ActorKeypair};

    fn empty_cfg() -> SuccessionLedger {
        SuccessionLedger::empty(ActorId([9u8; 32]))
    }

    /// `cfg` through the grant-mint door of a store that holds it — the only
    /// way to build [`PublishedGrants`].
    fn published(cfg: &SuccessionLedger) -> PublishedGrants {
        use fauna_client_config::SuccessionLedgerStore;
        let store = fauna_client_config::test_helpers::FakeSuccessionLedgerStore::with(cfg.clone());
        let ledger = fauna_client_testkit::block_on(
            store.merge_published(SuccessionLedger::empty(cfg.actor_id)),
        )
        .expect("the fake nest acknowledges the publish");
        PublishedGrants::from_published(&ledger)
    }

    fn mail_scope() -> GrantEventScope {
        GrantEventScope {
            class: "content.read".into(),
            kind: Some("mail".into()),
            tier: None,
        }
    }

    /// The reconcile sweep's three-way judgement, in one pin: an id the log
    /// never minted (a deposit whose `Mint` never became durable, or a row a
    /// hostile nest invented) and an id the log revoked (a row resurrected
    /// after revocation) are both unrecognized; a minted, un-revoked id is
    /// skipped by construction. `ui/nests.md` § Trust facet — grants →
    /// *Reconcile*: "revoke every returned id the log does not hold **live**
    /// (no event at all, or latest event `Revoke`)".
    #[test]
    fn unrecognized_is_the_orphan_and_the_resurrection_never_the_live_grant() {
        let mut cfg = empty_cfg();
        let kp = ActorKeypair::generate();
        let live = [1u8; 16];
        let resurrected = [2u8; 16];
        let orphan = [3u8; 16];

        record_mint(
            &mut cfg,
            kp.signing_key(),
            live,
            [0xAAu8; 32],
            vec![mail_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            resurrected,
            [0xAAu8; 32],
            vec![mail_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();
        record_revoke(&mut cfg, kp.signing_key(), resurrected, [0xAAu8; 32], 1500).unwrap();

        // The nest reports all three rows — the revoked one is back (a hostile
        // or buggy nest re-inserting after a revoke) and one was never logged.
        let unrecognized = unrecognized_grant_ids(&cfg, [live, resurrected, orphan]);
        assert_eq!(
            unrecognized,
            vec![resurrected, orphan],
            "the resurrection and the orphan are swept; the live grant is not"
        );
    }

    /// An id whose latest event is a `Renew` is still the log's business —
    /// renewal and expiry judgements stay log-side, never nest-side. This is
    /// the separate arm from the `Mint`-only case above: a fold that looked at
    /// mints alone would sweep every renewed grant off the nest.
    #[test]
    fn a_renewed_grant_is_recognized_expiry_is_not_the_sweeps_business() {
        let mut cfg = empty_cfg();
        let kp = ActorKeypair::generate();
        let renewed = [7u8; 16];
        record_mint(
            &mut cfg,
            kp.signing_key(),
            renewed,
            [0xBBu8; 32],
            vec![mail_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();
        record_renew(&mut cfg, kp.signing_key(), &renewed, 1000, 9000, 2500).unwrap();

        assert!(
            unrecognized_grant_ids(&cfg, [renewed]).is_empty(),
            "latest event Renew ⇒ the log holds it live"
        );
        // …and a window that has since closed changes nothing here: the sweep
        // judges recognition, not liveness-in-time. An expired-but-logged row
        // is exactly what `fauna.capabilities.reconcile` returns on purpose.
        assert!(
            unrecognized_grant_ids(&cfg, [renewed]).is_empty(),
            "expiry is judged log-side at render, never by the sweep"
        );
    }

    /// A hostile nest answering with more ids than an owner can possibly hold
    /// buys a bounded sweep, not an unbounded revoke storm: the reply is
    /// ≤ `MAX_GRANTS_PER_OWNER` by construction on an honest nest, so anything
    /// past that is truncated. Truncation is on what the client *processes*,
    /// deliberately before the filter — so the bound holds even when every id
    /// in the answer is unrecognized, which is the hostile case.
    #[test]
    fn a_hostile_oversized_answer_is_truncated_at_the_per_owner_cap() {
        let cfg = empty_cfg(); // empty log ⇒ every id is unrecognized
        let hostile: Vec<[u8; 16]> = (0..(MAX_GRANTS_PER_OWNER as u32 * 4))
            .map(|n| {
                let mut id = [0u8; 16];
                id[..4].copy_from_slice(&n.to_be_bytes());
                id
            })
            .collect();

        let swept = unrecognized_grant_ids(&cfg, hostile.clone());
        assert_eq!(
            swept.len(),
            MAX_GRANTS_PER_OWNER,
            "at most one per-owner cap's worth of revokes leaves one sweep"
        );
        assert_eq!(
            swept[0], hostile[0],
            "truncation keeps the answer's own order — it is a bound, not a filter"
        );
    }

    /// The cap bounds what the client **processes**, not what survives the
    /// filter — the two orders differ exactly here, and the ruling picks this
    /// one ("the client processes at most that many ids per sweep").
    ///
    /// An owner holding a full cap's worth of live grants leaves a nest with no
    /// room for another row, so every id past the cap in that answer is
    /// necessarily fabricated, and revoking a fabricated id is an idempotent
    /// no-op — nothing real is lost by not reaching them. What is gained is that
    /// a hostile nest cannot make the client do unbounded work by padding its
    /// answer. (Accepted residual, stated by the ruling: a *buggy* nest holding
    /// genuine rows past its own cap keeps its orphans past the cap unswept.)
    #[test]
    fn the_cap_bounds_ids_processed_not_revokes_emitted() {
        let mut cfg = empty_cfg();
        let kp = ActorKeypair::generate();
        let mut answer: Vec<[u8; 16]> = Vec::new();
        for n in 0..MAX_GRANTS_PER_OWNER as u32 {
            let mut id = [0u8; 16];
            id[..4].copy_from_slice(&n.to_be_bytes());
            record_mint(
                &mut cfg,
                kp.signing_key(),
                id,
                [0xAAu8; 32],
                vec![mail_scope()],
                1000,
                2000,
                1000,
            )
            .unwrap();
            answer.push(id);
        }
        // …then three ids the log never minted, past the cap.
        answer.extend([[0xF1u8; 16], [0xF2u8; 16], [0xF3u8; 16]]);

        assert!(
            unrecognized_grant_ids(&cfg, answer).is_empty(),
            "the first cap's worth of ids are all recognized, so this sweep \
             revokes nothing — the bound is on ids processed, and an id past a \
             full cap cannot name a real row"
        );
    }

    /// **Nothing from the answer enters client state.** The predicate takes
    /// `&SuccessionLedger`, so this is true by construction — the pin is here so that
    /// a future refactor to `&mut` (e.g. "let's record what we swept") fails a
    /// test that says why it must not: a `Revoke` event for an id the log never
    /// minted would be nest-influenced content entering the signed log, the
    /// precise channel the audit rule exists to close.
    #[test]
    fn the_sweep_never_writes_the_grant_log() {
        let mut cfg = empty_cfg();
        let kp = ActorKeypair::generate();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            [0xAAu8; 32],
            vec![mail_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();
        let before = cfg.grant_events.clone();

        let swept = unrecognized_grant_ids(&cfg, [[9u8; 16], [8u8; 16]]);
        assert_eq!(swept.len(), 2, "both unknown ids are swept");
        assert_eq!(
            cfg.grant_events, before,
            "the log is untouched — the sweep appends NO GrantEvent"
        );
    }

    #[test]
    fn bounded_marker_discriminates_and_survives_the_fold() {
        // The regime discriminator rides an EXISTING signed field
        // (`scope[].tier`) — the `GrantEvent` shape is frozen (a net-new field
        // breaks `verify()` on older devices and the merge then drops the
        // event). This pins: the marked scope signs + verifies, folds through
        // `current_grants`, and `is_bounded_mail_grant` separates it from a
        // master mail grant and from a non-mail scope.
        let mut cfg = empty_cfg();
        let kp = ActorKeypair::generate();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            [2u8; 32],
            vec![bounded_mail_event_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [3u8; 16],
            [4u8; 32],
            vec![mail_scope()], // master mail grant: no marker
            1000,
            2000,
            1000,
        )
        .unwrap();
        cfg.grant_events
            .iter()
            .for_each(|e| e.verify(&kp.actor_id()).expect("marked event verifies"));

        let current = current_grants(&cfg);
        let bounded: Vec<_> = current
            .iter()
            .filter(|g| is_bounded_mail_grant(&g.scope))
            .collect();
        assert_eq!(bounded.len(), 1, "exactly the marked grant is bounded");
        assert_eq!(bounded[0].grant_id, vec![1u8; 16]);
        assert!(!is_bounded_mail_grant(&[GrantEventScope {
            class: "content.read".into(),
            kind: Some("spam-model".into()),
            tier: Some("bounded".into()), // wrong kind: marker doesn't apply
        }]));
    }

    /// The per-labeler grant's log twin: both tuples carry the labeler's factor
    /// folded into `kind`, the grant still reads as bounded (the regime
    /// predicate looks through the fold), the factor reads back as one value,
    /// and `current_labeler_grant` finds exactly this grant by its labeler —
    /// never a plain mail grant, never another labeler's.
    #[test]
    fn labeler_scope_folds_the_factor_and_is_found_by_its_labeler() {
        let labeler = ActorId([0xAAu8; 32]);
        let other = ActorId([0xBBu8; 32]);
        let scope = bounded_mail_labeler_event_scope(&labeler);
        let factor = fauna_core::scoring::labeler_factor(&labeler);
        assert_eq!(scope.len(), 2, "mail read + label-write");
        assert!(scope.iter().all(|s| s.factor() == Some(factor.as_str())));
        assert_eq!(scope[0].base_kind(), Some(ScopeTuple::KIND_MAIL));
        assert_eq!(scope[0].tier.as_deref(), Some(GRANT_SCOPE_TIER_BOUNDED));
        assert_eq!(scope[1].class, ScopeTuple::CLASS_CONTENT_LABEL_WRITE);
        assert_eq!(scope[1].base_kind(), None);
        assert!(
            is_bounded_mail_grant(&scope),
            "a per-labeler grant is bounded too"
        );
        assert_eq!(
            labeler_factor_of_grant(&scope).as_deref(),
            Some(factor.as_str())
        );
        assert_eq!(labeler_factor_of_grant(&[bounded_mail_event_scope()]), None);
        assert_eq!(labeler_factor_of_grant(&[mail_scope()]), None);

        let mut cfg = empty_cfg();
        let kp = ActorKeypair::generate();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            [2u8; 32],
            scope,
            1000,
            2000,
            1000,
        )
        .unwrap();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [3u8; 16],
            [2u8; 32],
            vec![bounded_mail_event_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();
        cfg.grant_events
            .iter()
            .for_each(|e| e.verify(&kp.actor_id()).expect("folded event verifies"));

        let found = current_labeler_grant(&cfg, &labeler).expect("the labeler's grant");
        assert_eq!(found.grant_id, vec![1u8; 16]);
        assert!(current_labeler_grant(&cfg, &other).is_none());

        // Revoked ⇒ no longer the subscription's twin.
        record_revoke(&mut cfg, kp.signing_key(), [1u8; 16], [2u8; 32], 1500).unwrap();
        assert!(current_labeler_grant(&cfg, &labeler).is_none());
    }

    #[test]
    fn mint_then_current_grants_shows_it() {
        let mut cfg = empty_cfg();
        let kp = ActorKeypair::generate();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            [2u8; 32],
            vec![mail_scope()],
            1000,
            1000 + 90 * 24 * 60 * 60,
            1000,
        )
        .unwrap();

        let current = current_grants(&cfg);
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].grant_id, vec![1u8; 16]);
        assert_eq!(current[0].holder, vec![2u8; 32]);
        assert_eq!(current[0].scope, vec![mail_scope()]);
        assert_eq!(current[0].as_of, 1000);
    }

    #[test]
    fn revoke_removes_it_from_current_grants_but_not_history() {
        let mut cfg = empty_cfg();
        let kp = ActorKeypair::generate();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            [2u8; 32],
            vec![mail_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();
        record_revoke(&mut cfg, kp.signing_key(), [1u8; 16], [2u8; 32], 1500).unwrap();

        assert!(
            current_grants(&cfg).is_empty(),
            "revoked grant goes dark in Now"
        );
        let hist = history(&cfg);
        assert_eq!(hist.len(), 2, "History keeps the full timeline");
        assert_eq!(hist[0].kind, GrantEventKind::Revoke, "most-recent-first");
        assert_eq!(hist[1].kind, GrantEventKind::Mint);
    }

    /// Regression: a `Revoke` in the **same `at` second** as its `Mint` (a
    /// same-second mint→revoke, or a coarse clock) must still drop the grant.
    /// The fold decides "revoked" by a `Revoke`'s *presence*, not "the latest
    /// event by `(at, sig)`" — that tie-break is arbitrary. Both events are
    /// hand-built with the `Mint`'s `sig` bytes (all `0xFF`) **greater** than the
    /// `Revoke`'s (all `0x00`) — the ordering that resurrected the grant under
    /// the old logic; `current_grants` must still be empty, `history` keeps both.
    #[test]
    fn same_second_revoke_drops_grant_regardless_of_sig_tiebreak() {
        let mut cfg = empty_cfg();
        let gid = vec![7u8; 16];
        let holder = vec![2u8; 32];
        cfg.grant_events.push(GrantEvent {
            grant_id: gid.clone(),
            holder: holder.clone(),
            kind: GrantEventKind::Mint,
            scope: vec![mail_scope()],
            window_start: 1000,
            window_end: 5000,
            at: 1000,
            sig: vec![0xFF; 64], // greater than the revoke's — old code kept the mint
        });
        cfg.grant_events.push(GrantEvent {
            grant_id: gid,
            holder,
            kind: GrantEventKind::Revoke,
            scope: vec![],
            window_start: 0,
            window_end: 0,
            at: 1000, // same second as the mint
            sig: vec![0x00; 64],
        });
        assert!(
            current_grants(&cfg).is_empty(),
            "same-second revoke must still drop the grant (revocation is terminal)"
        );
        assert_eq!(history(&cfg).len(), 2, "History keeps both events");
    }

    #[test]
    fn renew_carries_scope_forward_and_extends_window() {
        let mut cfg = empty_cfg();
        let kp = ActorKeypair::generate();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            [2u8; 32],
            vec![mail_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();
        record_renew(&mut cfg, kp.signing_key(), &[1u8; 16], 1000, 9000, 1900).unwrap();

        let current = current_grants(&cfg);
        assert_eq!(current.len(), 1);
        assert_eq!(
            current[0].scope,
            vec![mail_scope()],
            "scope carried forward"
        );
        assert_eq!(current[0].window_start, 1000, "window_start unchanged");
        assert_eq!(current[0].window_end, 9000, "window_end extended");
        assert_eq!(current[0].as_of, 1900);
    }

    /// The retention ruling's client half: a renewal re-centres the window on
    /// the renewal instant (one mint-length behind, one ahead), the start
    /// never moves back, and the log refuses a backward start outright.
    #[test]
    fn renewal_window_recentres_and_the_log_refuses_a_backward_start() {
        let mut cfg = empty_cfg();
        let kp = ActorKeypair::generate();
        let day = 24 * 60 * 60;
        let len = 90 * day;
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            [2u8; 32],
            vec![mail_scope()],
            1000 * day,
            1000 * day + len,
            1000 * day,
        )
        .unwrap();
        let current = current_grants(&cfg).remove(0);
        // The first renewal, inside the renew-ahead threshold: `now - L` is
        // still behind the mint start, so the start stays put and only the
        // end moves — the window is L + (L - the lead) wide, under 2L.
        assert_eq!(
            renewal_window(&current, len, 1085 * day),
            (1000 * day, 1175 * day)
        );
        record_renew(
            &mut cfg,
            kp.signing_key(),
            &[1u8; 16],
            1000 * day,
            1175 * day,
            1085 * day,
        )
        .unwrap();
        let current = current_grants(&cfg).remove(0);
        assert_eq!(
            (current.window_start, current.window_end),
            (1000 * day, 1175 * day)
        );
        // The chosen duration still reads the Mint, not the current width.
        assert_eq!(minted_window_secs(&cfg, &[1u8; 16]), Some(len));
        // The second renewal: `now - L` has passed the start, which slides
        // to it — exactly 2L wide from here on, one length behind the
        // renewal instant and one ahead.
        assert_eq!(
            renewal_window(&current, len, 1170 * day),
            (1080 * day, 1260 * day)
        );
        let (s, e) = renewal_window(&current, len, 1170 * day);
        assert_eq!(e - s, 2 * len);
        record_renew(
            &mut cfg,
            kp.signing_key(),
            &[1u8; 16],
            1080 * day,
            1260 * day,
            1170 * day,
        )
        .unwrap();
        let current = current_grants(&cfg).remove(0);
        assert_eq!(
            (current.window_start, current.window_end),
            (1080 * day, 1260 * day)
        );
        // A start behind the recorded one is refused by the log itself.
        assert_eq!(
            record_renew(
                &mut cfg,
                kp.signing_key(),
                &[1u8; 16],
                1070 * day,
                1300 * day,
                1180 * day
            )
            .unwrap_err(),
            RenewError::WindowStartMovedBack
        );
    }

    #[test]
    fn renew_unknown_grant_errors() {
        let mut cfg = empty_cfg();
        let kp = ActorKeypair::generate();
        let err =
            record_renew(&mut cfg, kp.signing_key(), &[9u8; 16], 1000, 9000, 1000).unwrap_err();
        assert_eq!(err, RenewError::GrantNotFound);
    }

    #[test]
    fn renew_after_revoke_errors_grant_is_gone() {
        let mut cfg = empty_cfg();
        let kp = ActorKeypair::generate();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            [2u8; 32],
            vec![mail_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();
        record_revoke(&mut cfg, kp.signing_key(), [1u8; 16], [2u8; 32], 1500).unwrap();

        let err =
            record_renew(&mut cfg, kp.signing_key(), &[1u8; 16], 1000, 9000, 1900).unwrap_err();
        assert_eq!(err, RenewError::GrantNotFound);
    }

    #[test]
    fn every_event_verifies_against_the_owners_identity() {
        let mut cfg = empty_cfg();
        let kp = ActorKeypair::generate();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            [2u8; 32],
            vec![mail_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();
        record_renew(&mut cfg, kp.signing_key(), &[1u8; 16], 1000, 9000, 1900).unwrap();
        record_revoke(&mut cfg, kp.signing_key(), [1u8; 16], [2u8; 32], 2500).unwrap();

        for e in &cfg.grant_events {
            e.verify(&kp.actor_id())
                .expect("every appended event verifies against the minting identity");
        }
        // A different identity's key must NOT verify — proves the log is
        // forgery-proof by a box that doesn't hold the owner's identity key.
        let attacker = ActorKeypair::generate();
        for e in &cfg.grant_events {
            assert!(e.verify(&attacker.actor_id()).is_err());
        }
    }

    #[test]
    fn multiple_grants_fold_independently() {
        let mut cfg = empty_cfg();
        let kp = ActorKeypair::generate();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            [2u8; 32],
            vec![mail_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [3u8; 16],
            [4u8; 32],
            vec![GrantEventScope {
                class: "content.label-write".into(),
                kind: None,
                tier: None,
            }],
            1000,
            2000,
            1100,
        )
        .unwrap();
        record_revoke(&mut cfg, kp.signing_key(), [3u8; 16], [4u8; 32], 1200).unwrap();

        let current = current_grants(&cfg);
        assert_eq!(current.len(), 1, "only the non-revoked grant survives Now");
        assert_eq!(current[0].grant_id, vec![1u8; 16]);
        assert_eq!(
            history(&cfg).len(),
            3,
            "History keeps all events from both grants"
        );
        assert_eq!(history_for_grant(&cfg, &[3u8; 16]).len(), 2);
    }

    #[test]
    fn seam_path_build_sign_append_matches_record_mint() {
        // The machine wiring signs an unsigned event through the
        // `GrantEventSigner` seam (no raw key crossing FFI). Prove that path —
        // `build_mint_event` → external sign → `append_signed` — lands the same
        // current-grants state as the direct-key `record_mint`.
        let kp = ActorKeypair::generate();

        let mut direct = empty_cfg();
        record_mint(
            &mut direct,
            kp.signing_key(),
            [1u8; 16],
            [2u8; 32],
            vec![mail_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();

        let mut seam = empty_cfg();
        let unsigned = build_mint_event([1u8; 16], [2u8; 32], vec![mail_scope()], 1000, 2000, 1000);
        assert!(
            unsigned.verify(&kp.actor_id()).is_err(),
            "the built event is unsigned until the seam signs it"
        );
        let signed = unsigned.sign(kp.signing_key()).unwrap();
        signed
            .verify(&kp.actor_id())
            .expect("seam-signed event verifies against the owner");
        append_signed(&mut seam, signed);

        assert_eq!(
            current_grants(&direct),
            current_grants(&seam),
            "seam path and direct path fold to the same Now state"
        );
    }

    #[test]
    fn build_renew_event_is_unsigned_and_carries_scope_forward() {
        let mut cfg = empty_cfg();
        let kp = ActorKeypair::generate();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            [2u8; 32],
            vec![mail_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();

        let renew = build_renew_event(&cfg, &[1u8; 16], 1000, 5000, 1500).unwrap();
        assert_eq!(renew.kind, GrantEventKind::Renew);
        assert_eq!(renew.scope, vec![mail_scope()], "scope fixed at mint");
        assert_eq!(renew.window_start, 1000, "start carried forward");
        assert_eq!(renew.window_end, 5000, "window extended");
        assert!(
            renew.verify(&kp.actor_id()).is_err(),
            "build_renew_event returns an unsigned event for the seam"
        );

        assert!(
            build_renew_event(&cfg, &[9u8; 16], 1000, 5000, 1500).is_err(),
            "renewing an unknown grant errors"
        );
    }

    /// The happy path: an event that reached storage releases its blob.
    #[test]
    fn a_recorded_grant_releases_its_blob() {
        let kp = ActorKeypair::generate();
        let mut cfg = empty_cfg();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [3u8; 16],
            [2u8; 32],
            vec![mail_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();

        let pending = UndepositedGrant::new([3u8; 16], b"sealed-blob".to_vec());
        assert_eq!(pending.grant_id(), [3u8; 16]);
        let bytes = pending
            .release(&published(&cfg))
            .expect("the stored log records this mint");
        assert_eq!(bytes, b"sealed-blob".to_vec());
    }

    /// The bug this type exists to make unrepresentable: a blob whose `Mint`
    /// event is NOT in the stored log must not reach the nest, because nothing
    /// downstream could ever find it again — the page projects the log and
    /// `revoke` needs an id only the log carries.
    #[test]
    fn an_unrecorded_grant_refuses_to_be_deposited() {
        let cfg = empty_cfg();

        let err = UndepositedGrant::new([7u8; 16], b"sealed-blob".to_vec())
            .release(&published(&cfg))
            .expect_err("an empty log records no mint");

        assert_eq!(err.grant_id, "07070707070707070707070707070707");
        assert!(
            err.to_string().contains("neither show nor revoke"),
            "the error says what the user loses, not just that a lookup missed: {err}"
        );
    }

    /// The published form's whole point: a `Mint` merged locally but whose
    /// publish the bound nest did not acknowledge yields no proof at all, so
    /// no blob can be released behind it — a sibling replica could not yet
    /// read the event its reconcile sweep would judge the row by.
    #[test]
    fn a_publish_the_nest_refused_yields_no_proof() {
        use fauna_client_config::SuccessionLedgerStore;
        let kp = ActorKeypair::generate();
        let mut replica = SuccessionLedger::empty(kp.actor_id());
        record_mint(
            &mut replica,
            kp.signing_key(),
            [5u8; 16],
            [2u8; 32],
            vec![mail_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();
        let store =
            fauna_client_config::test_helpers::FakeSuccessionLedgerStore::empty(replica.actor_id);
        store.publish_refuses(true);

        let refused = fauna_client_testkit::block_on(store.merge_published(replica));

        assert!(refused.is_err(), "an unacknowledged publish is a refusal");
        assert_eq!(
            store.current().grant_events.len(),
            1,
            "the event stays recorded locally — the recoverable phantom-row direction"
        );
    }

    /// The check is per-grant, not "the log is non-empty" — a batch that
    /// recorded option 1 and lost option 2 must release exactly one blob.
    #[test]
    fn the_proof_is_per_grant_not_per_log() {
        let kp = ActorKeypair::generate();
        let mut cfg = empty_cfg();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [1u8; 16],
            [2u8; 32],
            vec![mail_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();
        let recorded = published(&cfg);

        assert!(
            UndepositedGrant::new([1u8; 16], b"one".to_vec())
                .release(&recorded)
                .is_ok()
        );
        assert!(
            UndepositedGrant::new([2u8; 16], b"two".to_vec())
                .release(&recorded)
                .is_err(),
            "a sibling grant in the same batch proves nothing about this one"
        );
    }

    /// A revoked grant still counts as recorded: the question is "can the user's
    /// app name it", and a revoked row is in History with an idempotent revoke.
    /// (Pinned because reaching for `latest_live_events` here — the fold that
    /// drops revokes — is the obvious wrong turn, and it would refuse to deposit
    /// a re-mint of a previously revoked id.)
    #[test]
    fn a_revoked_grant_still_counts_as_recorded() {
        let kp = ActorKeypair::generate();
        let mut cfg = empty_cfg();
        record_mint(
            &mut cfg,
            kp.signing_key(),
            [4u8; 16],
            [2u8; 32],
            vec![mail_scope()],
            1000,
            2000,
            1000,
        )
        .unwrap();
        record_revoke(&mut cfg, kp.signing_key(), [4u8; 16], [2u8; 32], 1500).unwrap();

        assert!(
            UndepositedGrant::new([4u8; 16], b"blob".to_vec())
                .release(&published(&cfg))
                .is_ok(),
            "a Mint event stays a Mint event in the log after a Revoke"
        );
    }
}
