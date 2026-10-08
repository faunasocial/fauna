//! The post-succession **aftermath**'s capability-grant re-mint — the leg of
//! the urgent sequence § Re-key scope requires of a successor's client where
//! getting the shape wrong is a *confidentiality* break, not a routing one
//! (`docs/goal/behavior/succession-aftermath.md` § Re-key scope, the
//! capability-grants row: *"Revoke all; successor re-mints from the grant
//! ledger …, each re-minted entry marked un-adjudicated"*).
//!
//! # What is actually broken until this runs
//!
//! The succession transaction deletes every grant the old identity minted —
//! `DELETE FROM capability_grants WHERE owner_actor_id = ?1`
//! (`bins/fauna-nest/src/db/successions.rs`) — so the box services those
//! grants empowered (the MDA's spam scoring, mail search, any
//! content-processor) silently stop being able to do their job. The owner's
//! own record of what was granted survives, though: the signed, append-only
//! grant-event log — the succession ledger's `event/…` rows
//! (`fauna.state.succession-ledger`), which the succession re-seals nothing of
//! and the successor reads once its post-store-ready pass re-pointed the chain.
//! This leg folds that log to its live grants and re-mints each one under the
//! successor identity.
//!
//! # Why the ledger is the *right* source, not merely the available one
//!
//! A pre-succession seed thief could mint grants directly with the nest and
//! deliberately leave them out of the owner's ledger — hiding them from the
//! audit surface. Those die with the succession transaction's revoke-all and,
//! because this leg re-mints **only what the ledger records**, they stay dead.
//! A thief could instead have *recorded* a grant in the ledger (they held the
//! seed, so they could sign as the owner); that one is re-minted — which is
//! exactly why every re-minted grant carries an un-adjudicated mark for the
//! owner to keep or remove (§ Adjudicating what the aftermath carries across).
//! The web-paywall folder grant is deliberately outside this leg: it is not
//! ledger-recorded and **fails closed** (the set darkens to its teaser until
//! an explicit owner paywall gesture re-mints it), so no silent authority is
//! restored and no mark is owed.
//!
//! # The idempotency device: the ledger is its own progress record
//!
//! Every event is signed by the identity that minted it. A live grant whose
//! latest event verifies against a **prior** owner (the ledger chain's retired
//! identities, recorded by the pass's chain re-point) still owes a re-mint; one
//! whose latest event verifies against the current owner is done. No progress
//! state at rest, safe to run at every store-ready on every device, and
//! deliberately not a one-flag check (the lesson: a one-flag check passes
//! every test you'd naturally write and is wrong).
//!
//! The replacement `grant_id` is **derived**, not random —
//! `blake3::derive_key` over the successor secret and the old id — so two
//! devices racing this pass, or a pass interrupted anywhere between its
//! ledger writes and its deposits, converge on ONE replacement grant: the
//! nest keys grants by `(owner, grant_id)` (mint is an idempotent upsert)
//! and the log fold collapses same-id mint events.
//!
//! # Ordering, holders, and what this refuses to do
//!
//! It runs in the **post-store-ready pass**
//! (`fauna_client_recovery::ledger_aftermath`), after the chain re-point and
//! the grant-mark raise: the ledger it re-mints from is the successor's account
//! store, which the post-auth pass cannot reach. The payloads a re-mint wraps
//! derive from that store's rows too — the MSEK from the mail custody
//! (`fauna.state.mail`), a post tier's key from the period-key custody
//! (`fauna.state.subscriptions`), both handed in — so the leg waits on no
//! re-key of a predecessor-sealed blob: a successor's device re-mints all the
//! same.
//!
//! **Record-then-deposit, over the batch** (`ui/nests.md` § Trust facet —
//! grants): the pass runs in
//! three phases — record every replacement's `Mint` and save it through the
//! grant-mint door (the bound nest acknowledges it), deposit each
//! blob through [`grant_log::UndepositedGrant::release`], and only then
//! record the old ids' revokes + carry the marks. Deferring the retirement is
//! what keeps a failed deposit retryable: the candidate predicate keys on the
//! OLD event staying latest-live-predecessor-signed, so the naive full
//! inversion (record revoke + mint first) would have made every failed
//! deposit permanently dark instead. The retirement's `record_revoke` is
//! log-only by design — the succession transaction already deleted every
//! predecessor-owned nest row, so the event is forensic truth, not a
//! narrowing RPC.
//!
//! Each holder's seal target is **re-resolved from the live roster**
//! (`fauna_client_bridges::discover_holders`), never from the log-recorded
//! pubkey alone: re-wrapping without the holder's current ML-KEM key would
//! PQ-downgrade a hybrid grant (the rotation-heal driver's rule,
//! `ui/nests.md`). A holder absent from the roster is skipped and reported as
//! still owed — never minted classical-blind.

use ed25519_dalek::SigningKey;
use fauna_client_bridges::{HolderInfo, MailAdminClient, discover_holders};
use fauna_client_config::{StoreError, SuccessionLedgerStore};
use fauna_core::data::{GrantUnattestedMark, MailConfig, Timestamp, UnattestedVerdict};
use fauna_core::grant_event::{GrantEvent, GrantEventKind, GrantEventScope, latest_live_events};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::localized::LocalizedText;
use fauna_core::progress::ProgressOutcome;
use fauna_core::succession_ledger::SuccessionLedger;
use fauna_mls::wrapped_blob::{GrantWindow, ScopeTuple};
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::grant_log::{self, is_bounded_mail_grant};
use crate::rpc::CapabilitiesClient;
use crate::{mint_bounded_mail_grant, mint_bounded_mail_labeler_grant, mint_grant};

/// What [`remint_capability_grants`] found. Mirrors the other aftermath legs'
/// outcome discipline: the arms that mean "no write happened" are distinct,
/// because a progress surface that collapsed them would report success while a
/// thief-visible capability plane stayed dark — or worse, stayed lit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantRemintOutcome {
    /// No live grant owes a re-mint: the owner never minted any, they have all
    /// expired or been revoked, or a previous pass already re-minted them (the
    /// idempotent arm — the fold found only successor-signed grants). Writes
    /// nothing.
    NothingToRemint,
    /// The pass ran. `reminted` grants were re-minted under the successor (and
    /// marked); `owed` live predecessor-era grants could not be re-minted this
    /// pass — a holder absent from the live roster, a payload this device
    /// cannot derive, or a mint that did not land — and retry at the next
    /// sign-in.
    Reminted { reminted: usize, owed: usize },
}

/// The re-mint pass as a **progress surface**, mirroring
/// `BackupRegrantProgress` arm for arm via the shared
/// [`fauna_core::progress::Passage`] — the projection all 7 apps render, so
/// the copy lives where the outcome lives and no app writes a `match` over
/// the outcome.
pub type GrantRemintProgress = fauna_core::progress::Passage<GrantRemintOutcome>;

/// i18n keys for [`GrantRemintProgress::status_line`] —
/// `settings.recovery_kit.*`, beside the other aftermath legs' lines.
const KEY_REMINT_RUNNING: &str = "settings.recovery_kit.grant_remint_running";
const KEY_REMINT_DONE: &str = "settings.recovery_kit.grant_remint_done";
const KEY_REMINT_PARTIAL: &str = "settings.recovery_kit.grant_remint_partial";
const KEY_REMINT_FAILED: &str = "settings.recovery_kit.grant_remint_failed";

/// `NothingToRemint` renders nothing ([`ProgressOutcome::settled_line`]
/// returns `None`) — it is what every sign-in after a completed pass (and
/// every ordinary identity) returns, and a permanent no-op line trains the
/// user past the one that matters. The done line deliberately points at
/// review: every re-minted grant is marked un-adjudicated (§ Adjudicating
/// what the aftermath carries across), and the mark renders on the Nests
/// page, not here.
impl ProgressOutcome for GrantRemintOutcome {
    const RUNNING_KEY: &'static str = KEY_REMINT_RUNNING;
    const FAILED_KEY: &'static str = KEY_REMINT_FAILED;

    fn settled_line(&self) -> Option<LocalizedText> {
        match self {
            Self::Reminted { owed: 0, .. } => Some(LocalizedText::key(KEY_REMINT_DONE)),
            // Progress was real AND the pass is unfinished — both are true,
            // the fifth partly-owed state the `__mls` leg established.
            Self::Reminted { .. } => Some(LocalizedText::key(KEY_REMINT_PARTIAL)),
            Self::NothingToRemint => None,
        }
    }

    /// Whether the pass is still owed — a grant this pass could not re-mint
    /// retries at the next store-ready. The caller's resume condition, exactly
    /// as the sibling legs define it.
    fn still_owed(&self) -> bool {
        match self {
            Self::Reminted { owed, .. } => *owed > 0,
            Self::NothingToRemint => false,
        }
    }
}

/// Failure from [`remint_capability_grants`] — the pass could not run or could
/// not record what it did. Per-grant mint failures are NOT here: they fold
/// into [`GrantRemintOutcome::Reminted::owed`] and retry next sign-in.
#[derive(Debug)]
pub enum GrantRemintError {
    /// Reading the succession ledger failed. Nothing was changed.
    Ledger(StoreError),
    /// The live holder roster could not be resolved. Nothing was changed —
    /// without it every mint would risk a classical-blind PQ downgrade.
    Roster(fauna_client_bridges::DiscoverHoldersError),
    /// A ledger write failed — either the pre-deposit save that records the
    /// replacement `Mint`s (nothing was deposited yet; the retry is clean) or
    /// the post-deposit retirement write (the replacements are live AND
    /// recorded; only the old ids' revokes and the mark moves re-run). In
    /// neither case can a deposit outrun its record, and the derived
    /// `grant_id` makes the next pass converge on the same replacement.
    Save(StoreError),
}

impl core::fmt::Display for GrantRemintError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Ledger(e) => write!(f, "reading the grant ledger: {e}"),
            Self::Roster(e) => write!(f, "resolving the grant holders: {e}"),
            Self::Save(e) => write!(f, "recording the re-minted grants: {e}"),
        }
    }
}

impl std::error::Error for GrantRemintError {}

/// The derived replacement `grant_id` for a re-mint of `old_grant_id` by the
/// holder of `owner_secret` — deterministic so concurrent devices and
/// crash-retries converge on one replacement grant (see the module docs; the
/// idiom is `folder_paywall_grant_id`'s).
fn remint_grant_id(owner_secret: &[u8; 32], old_grant_id: &[u8]) -> [u8; 16] {
    let mut material = Vec::with_capacity(32 + old_grant_id.len());
    material.extend_from_slice(owner_secret);
    material.extend_from_slice(old_grant_id);
    let full = blake3::derive_key("fauna.succession.grant_remint.grant_id.v1", &material);
    let mut id = [0u8; 16];
    id.copy_from_slice(&full[..16]);
    id
}

/// One live predecessor-era grant the pass must re-mint — the owned projection
/// of its folded latest event, taken before the ledger is written.
struct RemintCandidate {
    old_grant_id: [u8; 16],
    holder: [u8; 32],
    scope: Vec<GrantEventScope>,
    window_start: u64,
    window_end: u64,
    /// The retired identity whose signature the latest event verifies against
    /// — the raising event, used to (re)stamp the mark if leg 1's is missing.
    predecessor: ActorId,
}

/// Which live grants still owe a re-mint: not expired, and the latest event is
/// **predecessor-signed** (verifies against a prior owner, not the current
/// one). Successor-signed grants are done; grants verifying against nobody we
/// know are left alone (a dropped predecessor or corruption — minting a grant
/// whose provenance is unknown would be exactly the wrong reflex).
fn remint_candidates(ledger: &SuccessionLedger, owner: &ActorId, now: u64) -> Vec<RemintCandidate> {
    latest_live_events(ledger)
        .into_iter()
        .filter(|e| e.window_end >= now)
        .filter(|e| e.verify(owner).is_err())
        .filter_map(|e: &GrantEvent| {
            let predecessor = ledger
                .prior_actor_ids
                .iter()
                .find(|p| e.verify(p).is_ok())?;
            Some(RemintCandidate {
                old_grant_id: e.grant_id.as_slice().try_into().ok()?,
                holder: e.holder.as_slice().try_into().ok()?,
                scope: e.scope.clone(),
                window_start: e.window_start,
                window_end: e.window_end,
                predecessor: *predecessor,
            })
        })
        .collect()
}

/// Whether the event scope carries the keyless `content.label-write` tuple —
/// the bounded-mail composed-scorer shape (`mint_bounded_mail_grant`'s
/// `include_label_write`).
fn scope_includes_label_write(scope: &[GrantEventScope]) -> bool {
    scope
        .iter()
        .any(|s| s.class == ScopeTuple::CLASS_CONTENT_LABEL_WRITE)
}

/// The event scope mapped to the blob's `ScopeTuple`s for a generic
/// (non-bounded) re-mint. The event's `tier` passes through (a post-tier grant
/// genuinely carries one) and so does a factor folded into `kind`
/// (`GrantEventScope::factor` — the blob's own qualifier, unfolded); `set` is
/// always `None` — folder grants are not ledger-recorded (module docs) so no
/// event scope ever names a set.
fn event_scope_to_tuples(scope: &[GrantEventScope]) -> Vec<ScopeTuple> {
    scope
        .iter()
        .map(|s| ScopeTuple {
            class: s.class.clone(),
            kind: s.base_kind().map(str::to_string),
            tier: s.tier.clone(),
            set: None,
            factor: s.factor().map(str::to_string),
        })
        .collect()
}

/// The marks the replacement carries — every mark on the retired grant id,
/// re-keyed onto `new_id` with its verdict (the mark follows the row the Nests
/// page actually renders), or, when the old id holds none, a fresh `Open` mark
/// from the signature-verified predecessor rather than a silent hand-back.
///
/// EVERY mark on the old id, not the first: a grant carried across two
/// successions holds one mark per raising event, each with its own verdict,
/// and carrying only one would leave the rest under an id the Nests page no
/// longer renders, where they are neither askable nor closable. The old rows
/// stay (a mark row is kept, never deleted); the retired grant is revoked, so
/// nothing renders them. A retried retirement re-derives the same marks, and
/// the mark arm's verdict-precedence join keeps an answer the owner already
/// gave — a retry never re-asks.
fn carried_marks(
    ledger: &SuccessionLedger,
    old_id: &[u8],
    new_id: [u8; 16],
    predecessor: ActorId,
) -> Vec<GrantUnattestedMark> {
    let carried: Vec<GrantUnattestedMark> = ledger
        .unattested_grant_marks
        .iter()
        .filter(|m| m.grant_id == old_id)
        .map(|m| GrantUnattestedMark {
            grant_id: new_id.to_vec(),
            ..m.clone()
        })
        .collect();
    if carried.is_empty() {
        vec![GrantUnattestedMark {
            grant_id: new_id.to_vec(),
            predecessor,
            verdict: UnattestedVerdict::default(),
        }]
    } else {
        carried
    }
}

/// A replacement grant whose `Mint` is recorded (or already was, from a prior
/// interrupted pass) but whose blob has not yet shipped — phase 1's product,
/// phase 2's input.
struct PendingRemint {
    blob: grant_log::UndepositedGrant,
    old_grant_id: [u8; 16],
    holder: [u8; 32],
    predecessor: ActorId,
}

/// A replacement grant whose deposit landed — phase 3 retires its old id
/// (log-only revoke) and carries the marks onto the row the page will render.
struct Handover {
    old_grant_id: [u8; 16],
    new_id: [u8; 16],
    holder: [u8; 32],
    predecessor: ActorId,
}

/// Re-mint this successor's ledger-recorded capability grants — the
/// aftermath's grant leg. See the module docs for the full contract.
///
/// `ledger` is the successor's succession-ledger seam — the account-store
/// handle, read as this identity after the pass's chain re-point.
/// `mail` is the account's mail custody as this device reads it
/// (`fauna.state.mail`'s READ fold — the MSEK a mail or calendar grant
/// re-derives its payload from; the default when the store is unreachable, so
/// such a grant stays owed and retries).
/// `owner_secret` is the **successor's own** seed: the replacement grants are
/// minted, signed and recorded as the successor, and the retired identity's
/// key material is never touched.
///
/// `period_keys` is where a post-tier grant's period key is read
/// (`fauna.state.subscriptions` — the same account store as `ledger`);
/// `None`, or a store that cannot be read yet, leaves every post-tier grant
/// owed — it re-mints on a later pass that can read the custody, never under a
/// guessed key.
///
/// **Safe to call unconditionally, on every device, at every store-ready.** An
/// identity that never succeeded pays one ledger read and returns
/// [`GrantRemintOutcome::NothingToRemint`]; the custody read and the roster
/// RPC are only spent when something is genuinely owed.
pub async fn remint_capability_grants<R>(
    nest: R,
    owner_secret: [u8; 32],
    ledger: &dyn SuccessionLedgerStore,
    period_keys: Option<&dyn fauna_client_subscriptions::PeriodKeyStore>,
    mail: &MailConfig,
) -> Result<GrantRemintOutcome, GrantRemintError>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    let keypair = ActorKeypair::from_secret(owner_secret);
    let owner = keypair.actor_id();
    let current = ledger.load().await.map_err(GrantRemintError::Ledger)?;

    // An identity that never succeeded holds no prior owners: nothing in its
    // ledger can be predecessor-signed, so skip the fold's verify work too.
    if current.prior_actor_ids.is_empty() {
        return Ok(GrantRemintOutcome::NothingToRemint);
    }
    let now = Timestamp::now_secs().max(0) as u64;
    let candidates = remint_candidates(&current, &owner, now);
    if candidates.is_empty() {
        return Ok(GrantRemintOutcome::NothingToRemint);
    }
    // Read once for every candidate; unreadable is `None`, which leaves a
    // post-tier grant owed rather than minted under a guess.
    let custody = match period_keys {
        Some(store) => store.custody().await.ok(),
        None => None,
    };

    // Resolve the live holder roster once for all grants — the seal targets
    // are recomputed, never read from the log (no classical-blind downgrade).
    let holders: Vec<HolderInfo> = discover_holders(&MailAdminClient::new(nest.clone()))
        .await
        .map_err(GrantRemintError::Roster)?;

    let signing_key: &SigningKey = keypair.signing_key();
    let capabilities = CapabilitiesClient::new(nest);
    let mut owed = 0usize;

    // Phase 1 — record intents. Every replacement's successor-signed `Mint`
    // event is recorded BEFORE anything ships: record, publish, then deposit
    // (`ui/nests.md` § Trust facet — grants), whose invariant is that
    // whatever the nest ends up holding, the durable log — and the nest's
    // own copy of it — already names. The
    // old id's revoke and the mark carry are deliberately NOT here: the old
    // event staying latest-live-predecessor-signed is what keeps the
    // candidate selectable, so a failed deposit retries at the next
    // store-ready — recording the retirement first would end the retry
    // permanently (the log is append-only and `Revoke` terminal).
    let mut batch: Vec<PendingRemint> = Vec::new();
    let mut intents = SuccessionLedger::events_replica(owner, Vec::new());
    for c in &candidates {
        let new_id = remint_grant_id(&owner_secret, &c.old_grant_id);
        let window = GrantWindow(c.window_start, c.window_end);
        // Custody grants (W8.2 (account-data-plane.md § Workstreams)) build on their own arm, BEFORE the roster
        // gate: the holder is the ceremony-pinned custodian device key —
        // never on the bridge-holder roster — and the grant is keyless, so
        // there is no wrap and no PQ-downgrade question: the log's holder IS
        // the re-mint target. Only the keyless nest row + the event pair
        // re-mint here; the re-signed WITNESS (the successor's re-offer,
        // T13's succession bullet) is the ceremony's own leg, delivered over
        // the next custody session — until then the re-minted row admits
        // nothing, which is the fail-closed direction.
        let blob_bytes = if let Some(set) = crate::custody_grants::custody_set_from_scopes(&c.scope)
        {
            match crate::custody_grants::custody_remint_blob_bytes(
                &owner.0, &new_id, &c.holder, window, &set,
            ) {
                Ok(b) => b,
                Err(_) => {
                    owed += 1;
                    continue;
                }
            }
        } else {
            // The holder must be on the live roster: absent means we cannot
            // know its current ML-KEM key, and minting classical-blind would
            // PQ-downgrade a hybrid grant. It stays owed and retries.
            let Some(holder) = holders.iter().find(|h| h.pubkey == c.holder) else {
                owed += 1;
                continue;
            };
            // Re-derive the payloads from the custodies handed in (`mail`,
            // `custody`) and wrap to the resolved holder. The window is the ORIGINAL grant's — re-minting must
            // never extend what the owner consented to. Event scope is carried
            // VERBATIM (the bounded regime marker, any label-write tuple and a
            // folded labeler factor ride along).
            let labeler = grant_log::labeler_factor_of_grant(&c.scope)
                .and_then(|f| fauna_core::scoring::labeler_factor_id(&f));
            let blob = if let Some(labeler) = labeler {
                // A per-labeler grant re-mints per labeler: every tuple and
                // wrap confined to that factor, exactly as the owner's
                // subscription minted it — never the composed read.
                mint_bounded_mail_labeler_grant(
                    mail,
                    &owner.0,
                    &new_id,
                    &holder.pubkey,
                    holder.mlkem_ek.as_deref(),
                    window,
                    &labeler,
                )
            } else if is_bounded_mail_grant(&c.scope) {
                mint_bounded_mail_grant(
                    mail,
                    &owner.0,
                    &new_id,
                    &holder.pubkey,
                    holder.mlkem_ek.as_deref(),
                    window,
                    scope_includes_label_write(&c.scope),
                )
            } else {
                mint_grant(
                    custody.as_ref(),
                    mail,
                    &owner.0,
                    &new_id,
                    &holder.pubkey,
                    holder.mlkem_ek.as_deref(),
                    window,
                    &event_scope_to_tuples(&c.scope),
                )
            };
            let blob = match blob {
                Ok(b) => b,
                // A payload this device cannot derive (mail not enabled here,
                // a tier key held elsewhere). Owed, retries where the
                // material is.
                Err(_) => {
                    owed += 1;
                    continue;
                }
            };
            match blob.to_canonical_bytes() {
                Ok(b) => b,
                Err(_) => {
                    owed += 1;
                    continue;
                }
            }
        };
        // A prior interrupted pass may already have recorded this replacement
        // (the derived id makes retries converge) — never record a duplicate
        // Mint; the deposit below is an idempotent upsert either way.
        let already_recorded = current
            .grant_events
            .iter()
            .any(|e| e.kind == GrantEventKind::Mint && e.grant_id.as_slice() == new_id);
        if !already_recorded
            // A record that cannot be signed stays owed — depositing anyway
            // is exactly the orphan this ordering exists to prevent.
            && grant_log::record_mint(
                &mut intents,
                signing_key,
                new_id,
                c.holder,
                c.scope.clone(),
                c.window_start,
                c.window_end,
                now,
            )
            .is_err()
        {
            owed += 1;
            continue;
        }
        batch.push(PendingRemint {
            blob: grant_log::UndepositedGrant::new(new_id, blob_bytes),
            old_grant_id: c.old_grant_id,
            holder: c.holder,
            predecessor: c.predecessor,
        });
    }
    if batch.is_empty() {
        return Ok(GrantRemintOutcome::Reminted { reminted: 0, owed });
    }

    // The one write that must precede every deposit — durable here and
    // acknowledged by the bound nest (the grant-mint door). `PublishedGrants`
    // is read from what the seam answers the ledger now reads as, so a write
    // the READ fold does not admit is caught at release below rather than
    // becoming an orphan. When every replacement was already recorded by a
    // prior pass the join puts nothing, and the door still publishes: a prior
    // pass's event the nest never acknowledged releases no blob until it has.
    let published = ledger
        .merge_published(intents)
        .await
        .map_err(GrantRemintError::Save)?;
    let recorded = grant_log::PublishedGrants::from_published(&published);

    // Phase 2 — deposit. A refused deposit leaves a PHANTOM row: recorded,
    // visible on the Nests page, cleared by an idempotent revoke — and its
    // candidate still selectable, so the next store-ready retries into the
    // same derived id (nest-side mint is INSERT OR REPLACE on (owner,
    // grant_id)).
    let mut reminted = 0usize;
    let mut retire: Vec<Handover> = Vec::new();
    for p in batch {
        let PendingRemint {
            blob,
            old_grant_id,
            holder,
            predecessor,
        } = p;
        let new_id = blob.grant_id();
        let Ok(blob_bytes) = blob.release(&recorded) else {
            owed += 1;
            continue;
        };
        if capabilities.mint(blob_bytes).await.is_err() {
            owed += 1;
            continue;
        }
        reminted += 1;
        retire.push(Handover {
            old_grant_id,
            new_id,
            holder,
            predecessor,
        });
    }

    // Phase 3 — retire. Only now that the replacement is known-deposited does
    // the ledger record the succession's revocation of the old id and carry
    // the marks onto the row the Nests page will render. The revoke is
    // log-only by design: the succession transaction already deleted every
    // predecessor-owned nest row (`db/successions.rs`), so there is no row a
    // `capabilities.revoke` RPC could remove — the event is forensic truth
    // about the ceremony's revoke-all, not a narrowing instruction. A failure
    // here is safe: the replacements are live AND recorded; only this
    // retirement bookkeeping re-runs at the next store-ready.
    if !retire.is_empty() {
        let mut retirement = SuccessionLedger::events_replica(owner, Vec::new());
        for h in &retire {
            let _ = grant_log::record_revoke(
                &mut retirement,
                signing_key,
                h.old_grant_id,
                h.holder,
                now,
            );
            retirement.unattested_grant_marks.extend(carried_marks(
                published.ledger(),
                &h.old_grant_id,
                h.new_id,
                h.predecessor,
            ));
        }
        ledger
            .merge(retirement)
            .await
            .map_err(GrantRemintError::Save)?;
    }
    Ok(GrantRemintOutcome::Reminted { reminted, owed })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(id: u8, pred: u8, verdict: UnattestedVerdict) -> GrantUnattestedMark {
        GrantUnattestedMark {
            grant_id: vec![id; 16],
            predecessor: ActorId([pred; 32]),
            verdict,
        }
    }

    fn ledger_with(marks: Vec<GrantUnattestedMark>) -> SuccessionLedger {
        SuccessionLedger {
            unattested_grant_marks: marks,
            ..SuccessionLedger::empty(ActorId([1u8; 32]))
        }
    }

    /// A grant carried across **two** successions holds one mark per raising
    /// event, each with its own verdict. The re-mint re-keys the grant id, and
    /// it must carry **all** of them onto the replacement: a mark left only on
    /// the retired id sits under a row the Nests page no longer renders, where
    /// it can neither be asked nor closed.
    #[test]
    fn the_carry_takes_every_raising_events_mark_with_its_verdict() {
        let ledger = ledger_with(vec![
            mark(7, 2, UnattestedVerdict::Kept),
            mark(7, 3, UnattestedVerdict::Open),
            mark(8, 3, UnattestedVerdict::Open),
        ]);
        let carried = carried_marks(&ledger, &[7u8; 16], [9u8; 16], ActorId([3u8; 32]));
        assert_eq!(
            carried,
            vec![
                mark(9, 2, UnattestedVerdict::Kept),
                mark(9, 3, UnattestedVerdict::Open),
            ],
            "both raising events follow the row, each with its own verdict"
        );
    }

    /// A ledger whose grant was never marked has no mark to carry, and the
    /// re-mint stamps one fresh rather than silently handing back capability
    /// with nothing to review.
    #[test]
    fn a_grant_with_no_mark_is_stamped_by_the_carry() {
        let carried = carried_marks(
            &ledger_with(vec![]),
            &[7u8; 16],
            [9u8; 16],
            ActorId([3u8; 32]),
        );
        assert_eq!(carried, vec![mark(9, 3, UnattestedVerdict::Open)]);
    }

    /// A retried retirement (an interrupted pass whose marks had already been
    /// carried, and answered since) re-derives the same marks — and the join
    /// must keep the owner's answer: a retry never re-asks.
    #[test]
    fn a_retried_carry_never_reopens_an_answered_mark() {
        let mut stored = ledger_with(vec![
            mark(7, 3, UnattestedVerdict::Open),
            mark(9, 3, UnattestedVerdict::Kept),
        ]);
        let retry = SuccessionLedger {
            unattested_grant_marks: carried_marks(
                &stored,
                &[7u8; 16],
                [9u8; 16],
                ActorId([3u8; 32]),
            ),
            ..SuccessionLedger::empty(ActorId([1u8; 32]))
        };
        stored = stored.merge(&retry);
        assert!(
            !GrantUnattestedMark::any_open(&stored.unattested_grant_marks, &[9u8; 16]),
            "a retry must not re-ask an answered question: {:?}",
            stored.unattested_grant_marks
        );
    }
}
