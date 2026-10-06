//! The seed-initiated replacement window — the honest-RecoveryKey-loss arm
//! (`docs/goal/behavior/identity-succession.md` § The RecoveryKey →
//! *Replacement*).
//!
//! When the kit itself is lost, the seed alone may register a new one — but
//! only after an uncontested **30-day** window, loudly surfaced on every device
//! for its whole duration, and instantly vetoable by whoever holds the current
//! RecoveryKey. That asymmetry is the entire point: between two seed holders
//! there is no winner, so the delay exists to give the *real* kit holder a
//! chance to say no.
//!
//! Three surfaces come from here: the request (Settings/Security), the standing
//! banner ([`PendingReplacement`], polled off `replacement.status`), and the
//! veto ceremony (pre-identity — the owner contesting a thief's request has
//! only the phrase).

use fauna_core::data::Timestamp;
use fauna_core::identity::ActorId;
use fauna_core::identity::ActorKeypair;
use fauna_core::recovery::{
    RECOVERY_REPLACE_GRACE_SECS, RecoveryKey, RecoveryKeyRegistration, ReplacementVeto,
    SignedRecoveryKeyRegistration,
};
use fauna_mls::wrapped_blob::PredecessorSeed;
use fauna_protocol::{RpcErrorClass, RpcRequester};
use zeroize::Zeroizing;

use crate::error::{RecoveryError, Result};
use crate::kit::hex32;
use crate::nest::RecoveryClient;
use crate::restore::ParsedKit;

/// The 30-day window a seed-alone replacement waits out, re-exported so a UI
/// renders the same constant the nest enforces rather than hard-coding "30".
pub const REPLACE_GRACE_SECS: u64 = RECOVERY_REPLACE_GRACE_SECS;

/// A seed-alone replacement request that has been parked but not yet landed.
///
/// The kit secret is returned **now**, at request time, not when the window
/// closes: the user has to write it down while they are looking at the screen,
/// and there is no second chance to display it.
pub struct PendingKit {
    secret_hex: Zeroizing<String>,
    /// The public half that will be registered if the window closes uncontested.
    pub recovery_pubkey: [u8; 32],
    /// Unix seconds it lands if nobody vetoes.
    pub lands_at: i64,
    /// The parked request itself — public, and naming no nest, so the same
    /// record opens a window at every nest the identity is linked to
    /// ([`request_seed_alone_replacement_at`]).
    request: SignedRecoveryKeyRegistration,
}

impl PendingKit {
    /// The 64-hex recovery secret — display once, never persist.
    pub fn secret_hex(&self) -> &str {
        &self.secret_hex
    }
}

impl core::fmt::Debug for PendingKit {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PendingKit")
            .field("secret_hex", &"<redacted>")
            .field("lands_at", &self.lands_at)
            .finish_non_exhaustive()
    }
}

/// The standing banner's model and its countdown, shared with the alert's
/// projection: both live in `fauna_client_core::recovery_pending`, below the
/// account runtime's secondary leg, which reads the same window at every
/// linked nest.
pub use fauna_client_core::recovery_pending::{PendingReplacement, days_remaining_from};

/// Ask to replace a lost RecoveryKey using the identity seed alone. USER class.
///
/// Mints a fresh kit and parks its registration for the 30-day window. The
/// record carries **no** `prior_recovery_sig` — the prior key is what was lost —
/// which is exactly why it cannot take effect immediately.
///
/// The escrow blob is deliberately **not** re-put here: nothing has changed
/// yet, the currently registered key still seals the resting blob, and the nest
/// deletes that row only when the replacement actually lands. Re-putting is
/// owed *then* — and only the user can supply the key it seals to, see
/// [`reseal_escrow_with_held_kit`].
pub async fn request_seed_alone_replacement<R>(
    client: &RecoveryClient<R>,
    identity: &ActorKeypair,
) -> Result<PendingKit>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let actor_id = identity.actor_id();
    let head = client
        .chain_head(&actor_id)
        .await?
        .ok_or(RecoveryError::NotRegistered)?;

    let recovery = RecoveryKey::generate();
    let registration = RecoveryKeyRegistration {
        actor_id,
        recovery_pubkey: recovery.public(),
        seq: head.seq + 1,
        created_at: Timestamp::now(),
    };
    // `prior = None`: the seed-alone arm by construction.
    let signed = registration
        .sign(identity.signing_key(), &recovery, None)
        .map_err(|e| RecoveryError::Crypto(format!("signing the registration: {e}")))?;

    let lands_at = client.replacement_request(&signed).await?;
    tracing::info!(lands_at, "seed-alone RecoveryKey replacement requested");

    Ok(PendingKit {
        secret_hex: Zeroizing::new(recovery.to_hex()),
        recovery_pubkey: recovery.public(),
        lands_at,
        request: signed,
    })
}

/// Send the same seed-alone request to one **linked** nest, which parks it
/// and runs its own 30-day window (`identity-succession.md` § Enforcement on
/// the home nest → *Every nest the identity is linked to*, clause (c)): a
/// landed seed-alone link carries no prior-key signature, so it can never be
/// replayed to a nest that did not run a window for it. USER class — over an
/// owner-authenticated connection to that nest. Answers when that nest's
/// window lands; a replayed identical request keeps the clock the first one
/// started.
///
/// One call per linked nest, after [`request_seed_alone_replacement`] at the
/// bound nest. A nest that cannot be reached now is not lost: the runtime's
/// secondary leg re-sends the link once the bound nest has landed it
/// (`fauna_client_core::recovery_chain`, the `Owed` answer).
pub async fn request_seed_alone_replacement_at<R>(
    linked: &RecoveryClient<R>,
    pending: &PendingKit,
) -> Result<i64>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let lands_at = linked.replacement_request(&pending.request).await?;
    tracing::info!(
        lands_at,
        "seed-alone RecoveryKey replacement requested at a linked nest"
    );
    Ok(lands_at)
}

/// The authenticated actor's pending replacement, if any — the standing banner
/// read. USER class.
pub async fn pending_replacement<R>(
    client: &RecoveryClient<R>,
) -> Result<Option<PendingReplacement>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    Ok(client.replacement_status().await?.map(Into::into))
}

/// Cancel whatever replacement currently pends, on proof of the **current**
/// RecoveryKey. Pre-identity.
///
/// Returns `false` when nothing was pending — an idempotent success: the
/// vetoer's goal state holds either way, and they have already proven they hold
/// the key, so this is an honest answer rather than a probe.
///
/// The nonce is single-use, so a captured veto can never be replayed to cancel
/// a future honest replacement — which is why this is a challenge/response pair
/// rather than one bare signed message.
pub async fn veto_pending_replacement<R>(
    client: &RecoveryClient<R>,
    kit: &ParsedKit,
    actor_id: Option<ActorId>,
) -> Result<bool>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let actor_id = actor_id.or(kit.actor_id).ok_or_else(|| {
        RecoveryError::Malformed(
            "this recovery kit does not name an account — enter the account to protect".into(),
        )
    })?;

    let challenge = client.replacement_challenge(&actor_id).await?;
    let signature = ReplacementVeto::new(actor_id, challenge.nonce)
        .sign(&kit.recovery)
        .map_err(|e| RecoveryError::Crypto(format!("signing the veto: {e}")))?;

    let cancelled = client
        .replacement_veto(&actor_id, &challenge.nonce, &signature)
        .await?;
    tracing::info!(cancelled, actor = %hex32(&actor_id.0), "replacement veto submitted");
    Ok(cancelled)
}

/// Contest the pending window at **every** nest that may hold one — the bound
/// nest and each linked nest — with one held kit (`identity-succession.md`
/// § Enforcement on the home nest → *Every nest the identity is linked to*,
/// clause (c): the veto contests the window at every nest that holds one).
///
/// Each nest runs its own challenge, so each gets its own
/// [`veto_pending_replacement`]; pre-identity, so `nests` may be anonymous
/// connections — the owner contesting a thief's request may hold only the
/// phrase. A nest with nothing pending answers an honest `false`. Every nest
/// is tried whatever the others answered, so one unreachable nest never
/// leaves a window standing at the rest; the answers come back in `nests`'
/// order.
pub async fn veto_pending_replacement_everywhere<R>(
    nests: &[&RecoveryClient<R>],
    kit: &ParsedKit,
    actor_id: Option<ActorId>,
) -> Vec<Result<bool>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let mut answers = Vec::with_capacity(nests.len());
    for nest in nests {
        answers.push(veto_pending_replacement(nest, kit, actor_id).await);
    }
    answers
}

/// Re-seal and re-put the escrow blob with the kit the user holds — the repair
/// for the `RegisteredNoEscrow` state, and the only shape a landed re-put can
/// take in production.
///
/// **Owed, not optional — and user-prompted by construction (ratified
/// 2026-08-10).** The nest deletes the escrow row on any registration that
/// changes the registered pubkey (`identity-succession.md` § Seed escrow →
/// *Lifecycle on the nest*), so from that moment until this call the account
/// has *no* escrow at all: `escrow.fetch` answers `no_escrow`, and phrase-only
/// restore is unavailable. For the **seed-alone** arm the deletion happens at
/// the landing, 30 days after the request — and by § The RecoveryKey →
/// *Custody* no device holds the new key by then
/// ([`request_seed_alone_replacement`] showed it once and persisted nothing),
/// so no notification, feeder or observable can drive this call by itself: the
/// surface asks the user for the kit they kept, and this ceremony verifies it
/// and re-puts. (Sealing early, at request time, is not an escape — the old
/// kit is still the chain head for the whole window and `escrow.put` replaces
/// the row, so an early put would break phrase-restore *inside* the window.)
/// The same repair serves every other arrival at `RegisteredNoEscrow`, e.g. a
/// client that failed between a minting ceremony's registration and its put.
///
/// **The head check is load-bearing, not politeness.** The nest cannot look
/// inside the blob, so a re-put sealed to a *retired* kit would rest and
/// answer presence while no key that can pass the fetch challenge opens it —
/// the silently-bricked state the lifecycle deletion exists to prevent,
/// reintroduced by the repair itself. Refusing with
/// [`RecoveryError::KitNotCurrent`] before anything is sent keeps that state
/// unrepresentable.
///
/// `predecessors` is [`crate::create_kit`]'s additive section, resolved by the
/// caller from its registry. This arm is registry-only **by construction**: no
/// row rests (that is the state being repaired), so there is no resting
/// section to union with or destroy — "a kit with no predecessor section beats
/// no kit at all" applies here exactly as at first registration
/// (`identity-succession.md` § Seed escrow, the carrying rule's seed-alone
/// paragraph). ⚠ It still must never be hard-coded `&[]`: a successor inside
/// the corpus re-seal window repairs through this path too, and dropping its
/// registry-resolved section here is the same loss, one step later.
///
/// Runs over the **authenticated** connection (`escrow.put` is USER class).
/// Nothing lands on any `Err` — every refusal precedes the put, so a retry is
/// free.
pub async fn reseal_escrow_with_held_kit<R>(
    client: &RecoveryClient<R>,
    identity: &ActorKeypair,
    kit: &ParsedKit,
    predecessors: &[PredecessorSeed],
) -> Result<i64>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let actor_id = identity.actor_id();
    if kit.actor_id.is_some_and(|named| named != actor_id) {
        return Err(RecoveryError::Malformed(
            "this recovery kit names a different account".into(),
        ));
    }
    let head = client
        .chain_head(&actor_id)
        .await?
        .ok_or(RecoveryError::NotRegistered)?;
    if kit.recovery.public() != head.recovery_pubkey {
        return Err(RecoveryError::KitNotCurrent);
    }
    let updated_at =
        crate::kit::try_put_escrow(client, identity, &kit.recovery, predecessors).await?;
    tracing::info!(updated_at, "escrow blob re-put under the held kit");
    Ok(updated_at)
}
