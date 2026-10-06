//! The succession ceremony and the `superseded`-refusal projection
//! (`docs/goal/behavior/identity-succession.md` § The succession statement,
//! § Enforcement on the home nest, § Propagation).
//!
//! Two halves, for the two sides of a theft:
//!
//! - [`succeed_identity`] — the owner, holding the offline RecoveryKey, mints a
//!   successor and re-points the account. Runs **pre-identity**, because the
//!   thief can revoke every session and invoke the seed-signed lockout; a
//!   client that demanded a signed-in session here would reproduce exactly the
//!   lock-out the design exists to escape (`identity-succession.md:69`).
//! - [`SupersededNotice`] + [`resolve_successor`] — every *other* device of the
//!   fleet, which learns about the succession by being refused. The refusal
//!   names a successor; this crate **verifies** that claim against the chain
//!   rather than trusting it, because the nest is enforcer and distributor,
//!   never authorizer (`identity-succession.md:77`).

use fauna_core::data::Timestamp;
use fauna_core::identity::ActorId;
use fauna_core::identity::ActorKeypair;
use fauna_core::recovery::{
    ChainHead, IdentitySuccession, MAX_VERIFIED_CHAIN_LEN, RecoveryKey, SignedIdentitySuccession,
    VerifiedSuccession, verify_succession_against_chain,
};
use fauna_protocol::{RpcError, RpcErrorClass, RpcRequester};

use crate::error::{RecoveryError, Result};
use crate::kit::hex32;
use crate::nest::RecoveryClient;

/// What a client shows when the nest refuses it with `fauna.auth.superseded`.
///
/// The shared projection all 7 apps render, so the refusal stops presenting as
/// a generic sign-in failure and starts being the affordance
/// `identity-succession.md:81` describes — *"this identity was succeeded —
/// import the new identity"* — routed to the existing identity-import flow
/// (the same distribution as adding a second device).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupersededNotice {
    /// The successor the refusal named. **Claimed, not proven** — pass it
    /// through [`resolve_successor`] before treating it as fact.
    pub claimed_successor: ActorId,
    /// The pre-identity kind that serves the statement proving the claim, named
    /// by the refusal itself so no client needs out-of-band knowledge.
    pub statement_kind: &'static str,
}

impl SupersededNotice {
    /// Read a notice out of a wire error, or `None` if it is not a superseded
    /// refusal.
    ///
    /// Keyed on `RpcError::superseded_by`, which returns `None` for every other
    /// code — so this can be called on any error without risk of pulling a
    /// successor out of an unrelated refusal.
    pub fn from_error(err: &RpcError) -> Option<Self> {
        err.superseded_by().map(|id| Self {
            claimed_successor: ActorId(id),
            statement_kind: RpcError::SUCCESSION_LOOKUP_KIND,
        })
    }

    /// The successor as 64-hex — what an import screen prefills.
    pub fn successor_hex(&self) -> String {
        hex32(&self.claimed_successor.0)
    }
}

/// The outcome of a completed succession.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuccessionOutcome {
    /// The identity the account now belongs to.
    pub new_actor_id: ActorId,
    /// Unix seconds the nest applied it.
    pub succeeded_at: i64,
    /// The statement that was landed, exactly as signed.
    ///
    /// Handed back because the client-side aftermath needs it and this is the
    /// only place it exists without a round trip: the per-group sweep
    /// ([`crate::sweep_groups`]) posts its **verbatim canonical bytes**
    /// in-group between the two commits, and re-encoding is what that carrier
    /// forbids. Re-fetching it via `succession.lookup` would work and cost a
    /// round trip per call site; returning it costs nothing, since the value
    /// is already in scope at submit.
    pub statement: SignedIdentitySuccession,
}

/// Re-point `old_actor_id` to a freshly minted successor identity.
///
/// The caller supplies the successor keypair rather than having it minted here,
/// because the successor's seed is the thing the user must keep — the same
/// value an identity-creation flow hands them — and this crate never persists
/// key material.
///
/// `old_identity` is the old signing key when the owner still holds it (the
/// theft case, and the loss case after an escrow restore). Passing it changes
/// no consumer's verdict — `old_sig` is never load-bearing
/// (`identity-succession.md:61`) — so `None` is always acceptable.
///
/// Runs over an **anonymous** connection to the old identity's home nest.
///
/// ## What this does *not* do
///
/// Succession is one atomic nest-side transaction, but the client-side aftermath
/// is not: the successor still has to register a fresh kit and put a fresh
/// escrow blob (the old identity's blob is deleted inside the transaction —
/// `identity-succession.md:100`), re-seal the `BackupKey` corpus, re-mint
/// capability grants, and drive the per-group MLS add-successor/remove-old
/// ceremony. Those need a signed-in session **as the successor** and are their
/// own tracks; this function stops at the statement landing.
///
/// ## An `Err` from here does **not** mean nothing landed
///
/// The nest commits the succession *before* it replies, so a transport fault in
/// that gap surfaces here as an `Err` over an account that has already moved.
/// This primitive can afford to say so bluntly because its caller minted the
/// successor and still holds it; [`succeed_with_held_kit`], which mints
/// internally, cannot, and returns [`SuccessionAttempt`] instead. Either way the
/// authoritative answer is [`reconcile_succession`] — never a retry, which
/// `AlreadySucceeded` refuses by construction.
pub async fn succeed_identity<R>(
    client: &RecoveryClient<R>,
    old_actor_id: ActorId,
    recovery: &RecoveryKey,
    successor: &ActorKeypair,
    old_identity: Option<&ActorKeypair>,
) -> Result<SuccessionOutcome>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let signed = author_succession(client, old_actor_id, recovery, successor, old_identity).await?;
    let (new_actor_id, succeeded_at) = client.submit_succession(&signed).await?;
    tracing::info!(
        old = %hex32(&old_actor_id.0),
        new = %hex32(&new_actor_id.0),
        "identity succeeded"
    );
    Ok(SuccessionOutcome {
        new_actor_id,
        succeeded_at,
        statement: signed,
    })
}

/// Everything [`succeed_identity`] does *before* it submits: read the chain,
/// check the kit against it, build and sign the statement.
///
/// Split out because the submit is the one step whose failure is **ambiguous**.
/// Nothing here has reached the nest, so every failure it can produce provably
/// left the account where it was — which is what lets [`succeed_with_held_kit`]
/// keep a wrong kit or an unreachable nest as an ordinary `Err` and reserve its
/// [`SuccessionAttempt::Unconfirmed`] arm for the genuinely unknown case.
async fn author_succession<R>(
    client: &RecoveryClient<R>,
    old_actor_id: ActorId,
    recovery: &RecoveryKey,
    successor: &ActorKeypair,
    old_identity: Option<&ActorKeypair>,
) -> Result<SignedIdentitySuccession>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let head = client
        .chain_head(&old_actor_id)
        .await?
        .ok_or(RecoveryError::NotRegistered)?;

    // Local pre-check: signing a statement under a kit the chain does not name
    // produces a record the nest must refuse. Catching it here lets the screen
    // say "that is not this account's kit" rather than relaying a refusal.
    if recovery.public() != head.recovery_pubkey {
        return Err(RecoveryError::PriorKitMismatch);
    }

    let statement = IdentitySuccession {
        old_actor_id,
        new_actor_id: successor.actor_id(),
        // The RecoveryKey registered for the OLD identity — the authorizer,
        // carried so a consumer holding the binding verifies with no fetch.
        recovery_pubkey: head.recovery_pubkey,
        // One monotonic sequence per identity, shared with registrations.
        seq: head.seq + 1,
        created_at: Timestamp::now(),
    };
    statement
        .sign(
            recovery,
            successor.signing_key(),
            old_identity.map(|k| k.signing_key()),
        )
        .map_err(|e| RecoveryError::Crypto(format!("signing the succession statement: {e}")))
}

/// What the "my identity was stolen" screen needs back from the ceremony.
///
/// Carries the successor's **secret** seed because the caller must persist it
/// before anything else: the account now belongs to a key that, at the instant
/// [`succeed_identity`] returns, exists nowhere but this process. Dropping it
/// unpersisted is the *client-only-resident key material* the no-user-data-loss
/// invariant names by name — an account nobody holds the key to.
pub struct SuccessionHandoff {
    /// The successor identity's 64-hex secret — persist it, then sign in with
    /// it. Zeroizing, for [`crate::RecoveryKit::secret_hex`]'s reason.
    successor_secret_hex: zeroize::Zeroizing<String>,
    /// The identity the account now belongs to.
    pub new_actor_id: ActorId,
    /// Unix seconds the nest applied it, when the submit reply carried it.
    ///
    /// `None` on a handoff rebuilt by [`reconcile_succession`]: the submit reply
    /// is where that value lives, and reconciliation exists precisely because
    /// that reply never arrived. The honest answer is "not known here" rather
    /// than the statement's own `created_at`, which is the *client's* clock —
    /// and `succession.lookup`, the only kind this reconcile can reach, serves
    /// statements rather than application stamps.
    ///
    /// ⚠ **`None` is not a dead end for the consumer.** The stamp *is* served,
    /// by `fauna.recovery.succession.status` — but that kind is authenticated
    /// and self-scoped by design (a server-observed commit stamp on the
    /// pre-identity `lookup` would be an anonymous "when was this account
    /// compromised and recovered" oracle), and every caller of this ceremony
    /// reaches it over an **anonymous** connector, which is the whole reason
    /// the ceremony works for an owner a thief has locked out. So the value is
    /// obtained where a signed-in connection already exists:
    /// `fauna_client_config::raise_succession_filter_marks`, the one consumer
    /// of the bound, asks for it whenever it is handed `None`.
    pub succeeded_at: Option<i64>,
    /// The landed statement, verbatim — see [`SuccessionOutcome::statement`].
    ///
    /// A screen driving this ceremony must carry it into the post-succession
    /// sweep: [`crate::sweep_groups`] refuses a statement whose pair does not
    /// match the two engines, so it is not reconstructible from the actor ids
    /// alone.
    pub statement: SignedIdentitySuccession,
}

impl SuccessionHandoff {
    /// The successor's 64-hex secret seed.
    pub fn successor_secret_hex(&self) -> &str {
        &self.successor_secret_hex
    }
}

impl core::fmt::Debug for SuccessionHandoff {
    /// Redacted — this holds an identity seed, which *is* the account.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SuccessionHandoff")
            .field("successor_secret_hex", &"<redacted>")
            .field("new_actor_id", &self.new_actor_id)
            .field("succeeded_at", &self.succeeded_at)
            // Not redacted: the statement is served pre-identity by
            // `succession.lookup` to anyone who asks. It is a public record.
            .field("statement", &self.statement)
            .finish()
    }
}

/// A succession whose submit failed **after** a successor was minted, so whether
/// it landed is genuinely unknown.
///
/// This type exists because the nest commits the succession before it replies
/// (`record_succession`'s `tx.commit()` precedes `encode_reply`), so a dropped
/// websocket in that gap is indistinguishable — from the client — from a submit
/// that never arrived. In one of those two worlds the account already belongs to
/// the key carried here, and it is the only copy in existence: dropping it is
/// the *client-only-resident key material* the no-user-data-loss invariant names
/// by name, and the account, handle, admin role and data become unrecoverable by
/// anyone, since by product invariant there is no operator to appeal to.
///
/// A caller therefore owes two things, in this order: **persist
/// [`successor_secret_hex`](Self::successor_secret_hex), then call
/// [`reconcile_succession`]** to find out which world it is in. A retry is not
/// an option in either — if it landed, the nest refuses the second attempt
/// `AlreadySucceeded`, and if it did not, the retry mints a fresh successor
/// anyway.
///
/// It deliberately does **not** carry the statement this client authored. The
/// bytes it signed are not evidence the nest accepted them, and a caller holding
/// them would be one step from treating an unconfirmed ceremony as done;
/// reconciliation re-fetches the statement from the nest and verifies it against
/// the registration chain, which is the only form that *is* evidence.
pub struct UnconfirmedSuccession {
    /// The successor identity's 64-hex secret — persist it first, ask second.
    successor_secret_hex: zeroize::Zeroizing<String>,
    /// The identity the account **may** now belong to.
    pub successor_actor_id: ActorId,
    /// The identity the ceremony was run for — what [`reconcile_succession`]
    /// looks up.
    pub old_actor_id: ActorId,
    /// Why the submit failed. Rendered by the surface once reconciliation has
    /// established that nothing landed; until then it describes the transport,
    /// not the outcome.
    pub error: RecoveryError,
}

impl UnconfirmedSuccession {
    /// The successor's 64-hex secret seed.
    pub fn successor_secret_hex(&self) -> &str {
        &self.successor_secret_hex
    }
}

impl core::fmt::Debug for UnconfirmedSuccession {
    /// Redacted — this holds an identity seed, which *is* the account.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UnconfirmedSuccession")
            .field("successor_secret_hex", &"<redacted>")
            .field("successor_actor_id", &self.successor_actor_id)
            .field("old_actor_id", &self.old_actor_id)
            .field("error", &self.error)
            .finish()
    }
}

/// What [`succeed_with_held_kit`] returns once a successor has been minted.
///
/// Both arms carry the successor seed, which is the whole point: the `Err` that
/// used to stand in for the second arm dropped it, and the account it may
/// already own with it.
#[derive(Debug)]
pub enum SuccessionAttempt {
    /// The nest confirmed the succession. The account has moved.
    Confirmed(SuccessionHandoff),
    /// The submit failed after the mint; whether the account moved is unknown
    /// until [`reconcile_succession`] answers.
    Unconfirmed(UnconfirmedSuccession),
}

impl SuccessionAttempt {
    /// The successor's 64-hex secret seed — **present on both arms, and the
    /// first thing a caller must persist.**
    ///
    /// Reachable without matching on purpose: "persist the seed before anything
    /// that can fail or block" is the one obligation that does not depend on
    /// which arm this is, and seven surfaces re-deriving that from a `match` is
    /// seven chances to persist it on only one of them.
    pub fn successor_secret_hex(&self) -> &str {
        match self {
            Self::Confirmed(handoff) => handoff.successor_secret_hex(),
            Self::Unconfirmed(unconfirmed) => unconfirmed.successor_secret_hex(),
        }
    }

    /// The successor identity — confirmed on one arm, claimed on the other.
    pub fn successor_actor_id(&self) -> ActorId {
        match self {
            Self::Confirmed(handoff) => handoff.new_actor_id,
            Self::Unconfirmed(unconfirmed) => unconfirmed.successor_actor_id,
        }
    }
}

/// The whole "my identity was stolen" ceremony, from the phrase the user pasted
/// to the successor seed their client must adopt — composed once here so all 7
/// apps drive one implementation (priority #2).
///
/// [`succeed_identity`] takes an already-parsed [`RecoveryKey`] and an
/// already-minted successor because it is the primitive; every *screen* has the
/// same three steps in front of it (parse the held kit, mint a successor, run
/// the ceremony), and a screen that composed them itself would be re-deriving
/// the minting rule — that the successor is generated **client-side and only
/// here**, never handed out by a nest — on seven surfaces.
///
/// `kit_input` is whatever the user pasted or scanned: the shared grammar
/// (bare 64-hex · `fauna://recovery` · colon form) is parsed here, so a
/// malformed phrase costs no round trip.
///
/// ## The seed survives a failed submit — that is what the return type is for
///
/// This function mints the successor, so it is the one place a caller *cannot*
/// hold the seed ahead of the submit. It therefore never returns an `Err` that
/// could have destroyed one: the outer `Err` covers only the steps that provably
/// never reached the nest (a malformed phrase, a kit naming another account, an
/// unreadable chain, a kit the chain does not name, a local signing fault), and
/// **every** submit failure comes back as
/// [`SuccessionAttempt::Unconfirmed`] carrying the seed.
///
/// The boundary is the submit call itself rather than the mint, and it is drawn
/// there deliberately: before the submit nothing can have committed, so the
/// minted seed provably authorizes nothing and dropping it costs the user
/// nothing — while a wrong kit keeps saying "that is not this account's kit"
/// instead of handing the screen an unconfirmed ceremony to explain. Past the
/// submit no classification is attempted at all, not even for a refusal that
/// looks definitive: the failure mode of guessing wrong is the account, so the
/// only safe rule is that the nest, asked again, is what decides
/// ([`reconcile_succession`]).
///
/// ## What the caller still owes
///
/// **Persisting [`SuccessionAttempt::successor_secret_hex`] is not optional, on
/// either arm.** Past that, this stops exactly where [`succeed_identity`] does —
/// at the statement landing. The successor still owes a fresh kit and a fresh
/// escrow blob (the old identity's was deleted inside the succession
/// transaction), a `BackupKey` re-seal, re-minted capability grants, and the
/// per-group MLS add-successor/remove-old ceremony. Those need a signed-in
/// session **as the successor** and are their own tracks
/// (`succession-aftermath.md` § Re-key scope); a screen driving this must say so
/// rather than implying the account is fully restored.
pub async fn succeed_with_held_kit<R>(
    client: &RecoveryClient<R>,
    old_actor_id: ActorId,
    kit_input: &str,
    old_identity: Option<&ActorKeypair>,
) -> Result<SuccessionAttempt>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    // Parse first: a malformed phrase must cost no round trip, and must not
    // mint a successor keypair that then has nothing to authorize it.
    let parsed = crate::restore::parse_kit(kit_input)?;
    // A kit that names an account names *which* one authoritatively, so a
    // phrase for a different account is caught here rather than surfacing as
    // `PriorKitMismatch` — which would tell the user their kit is stale when
    // in fact it is simply someone else's.
    if let Some(named) = parsed.actor_id
        && named != old_actor_id
    {
        return Err(RecoveryError::PriorKitMismatch);
    }
    let successor = ActorKeypair::generate();
    // Still pre-submit: an `Err` here cannot have moved the account, so the seed
    // minted a line ago is worthless and dropping it loses nothing.
    let signed = author_succession(
        client,
        old_actor_id,
        &parsed.recovery,
        &successor,
        old_identity,
    )
    .await?;

    // ── the boundary ────────────────────────────────────────────────────────
    // Past this call the nest may have committed, so no failure may take the
    // seed with it. This is the `?` the finding was about.
    match client.submit_succession(&signed).await {
        Ok((new_actor_id, succeeded_at)) => {
            tracing::info!(
                old = %hex32(&old_actor_id.0),
                new = %hex32(&new_actor_id.0),
                "identity succeeded"
            );
            Ok(SuccessionAttempt::Confirmed(SuccessionHandoff {
                successor_secret_hex: zeroize::Zeroizing::new(hex32(successor.secret_bytes())),
                new_actor_id,
                succeeded_at: Some(succeeded_at),
                statement: signed,
            }))
        }
        Err(error) => {
            tracing::warn!(
                old = %hex32(&old_actor_id.0),
                minted = %hex32(&successor.actor_id().0),
                %error,
                "succession submit failed after the successor was minted — outcome unknown, \
                 the seed must be persisted and the succession reconciled"
            );
            Ok(SuccessionAttempt::Unconfirmed(UnconfirmedSuccession {
                successor_secret_hex: zeroize::Zeroizing::new(hex32(successor.secret_bytes())),
                successor_actor_id: successor.actor_id(),
                old_actor_id,
                error,
            }))
        }
    }
}

/// Follow an identity's succession path to its terminal successor, verifying
/// every hop — the "verify the claim, don't trust it" half of § Propagation.
///
/// Returns `None` when the identity was never succeeded (an empty lookup is the
/// honest answer, not an error).
///
/// `client` must be connected to a nest the caller **already trusts for this
/// identity** — its home nest. That is the anchoring contract
/// `verify_registration_chain` documents: a chain delivered by the party
/// asserting the succession proves seed possession, never RecoveryKey
/// legitimacy, so it may be a hint to go fetch and never the anchor itself.
///
/// `known_head` is the chain head this consumer has previously seen for
/// `old_actor_id` — from a cached `Profile.recovery_head`, a persisted head
/// row, or an earlier walk — and **when the consumer holds one it must supply
/// it**: supplying it is what refuses a chain that *rewrites or truncates*
/// what was already seen (pass `None` only for genuine first contact, which
/// gets TOFU-grade assurance). The profile mirrors the whole coupled head for
/// exactly this call (the pubkey-only mirror left this guard
/// unreachable from a cached profile).
pub async fn resolve_successor<R>(
    client: &RecoveryClient<R>,
    old_actor_id: ActorId,
    known_head: Option<ChainHead>,
) -> Result<Option<VerifiedSuccession>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    Ok(walk_succession_path(client, old_actor_id, known_head)
        .await?
        .pop()
        .map(|(step, _statement)| step))
}

/// [`resolve_successor`]'s walk with **every** verified hop returned, oldest
/// first — empty when the identity was never succeeded. For a consumer that
/// must recognize an *intermediate* successor, not only where the account ended
/// up: a community room's policy chain may carry a version signed by a seat
/// since succeeded again (`conversation-rooms.md` § Roles and authorization →
/// *A name designates its verified line*). Same anchoring contract, same
/// `known_head` duty.
pub async fn resolve_succession_line<R>(
    client: &RecoveryClient<R>,
    old_actor_id: ActorId,
    known_head: Option<ChainHead>,
) -> Result<Vec<VerifiedSuccession>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    Ok(walk_succession_path(client, old_actor_id, known_head)
        .await?
        .into_iter()
        .map(|(step, _statement)| step)
        .collect())
}

/// The verified walk [`resolve_successor`], [`resolve_succession_line`] and
/// [`reconcile_succession`] run, oldest hop first, empty when the identity was
/// never succeeded.
///
/// Each hop is returned with the statement that carried it, because the two
/// callers need different halves of the same walk: one wants only where the
/// account ended up, the other wants the **verbatim bytes** of one specific hop
/// (`sweep_groups` refuses a re-encoded statement, so nothing downstream can
/// reconstruct them from the ids).
async fn walk_succession_path<R>(
    client: &RecoveryClient<R>,
    old_actor_id: ActorId,
    known_head: Option<ChainHead>,
) -> Result<Vec<(VerifiedSuccession, SignedIdentitySuccession)>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let statements = client.succession_lookup(&old_actor_id).await?;
    if statements.is_empty() {
        return Ok(Vec::new());
    }
    if statements.len() > MAX_VERIFIED_CHAIN_LEN {
        return Err(RecoveryError::Malformed(format!(
            "succession path longer than {MAX_VERIFIED_CHAIN_LEN} hops"
        )));
    }

    let mut current = old_actor_id;
    let mut head = known_head;
    let mut walked = Vec::with_capacity(statements.len());

    for statement in statements {
        // Each link is authorized by the RecoveryKey registered for the identity
        // *that link* succeeds, so the chain is re-fetched per hop rather than
        // reused — the hop-2 authorizer is a key the hop-1 identity never held.
        if statement.statement.old_actor_id != current {
            return Err(RecoveryError::Malformed(
                "succession path is not contiguous".into(),
            ));
        }
        let chain = client.registration_chain(&current).await?;
        let step = verify_succession_against_chain(&statement, &chain, head.as_ref())
            .map_err(|e| RecoveryError::Crypto(format!("verifying the succession: {e}")))?;
        current = step.new_actor_id;
        // A verified hop establishes no head for the *successor* identity — that
        // is a different chain, learned on the next hop's own fetch.
        head = None;
        walked.push((step, statement));
    }
    Ok(walked)
}

/// What the nest says about a succession whose reply never arrived.
#[derive(Debug)]
pub enum ReconciledSuccession {
    /// It landed, and the account belongs to the successor seed we hold. The
    /// handoff is the same one a confirmed ceremony would have produced, so the
    /// caller finishes exactly as it would have (sweep, then adopt).
    Landed(Box<SuccessionHandoff>),
    /// No succession is recorded for this identity: the submit genuinely never
    /// committed, the successor seed authorizes nothing, and the account is
    /// still the old identity's. The ceremony may be run again from the kit.
    NotLanded,
    /// A succession landed, but not ours — another holder of the same kit got
    /// there first (`old_actor_id` is the succession table's primary key, so
    /// first-succession-wins is structural). The seed we hold is dead; the
    /// account is reachable only by whoever holds `new_actor_id`.
    LandedForAnother {
        /// The successor the chain actually authorizes.
        new_actor_id: ActorId,
    },
}

/// Ask the nest whether a succession we could not confirm actually landed, and
/// hand back a finishable ceremony if it did.
///
/// This is the other half of [`SuccessionAttempt::Unconfirmed`]: the arm keeps
/// the seed, and this call turns it back into either a completable succession or
/// a definite "nothing happened". Without it the caller has a secret and no
/// instruction, which is barely better than the dropped seed it replaced.
///
/// It works from the persisted seed alone — no in-memory ceremony state — so it
/// serves the crash case as well as the in-session one: an app that died between
/// the submit and the reply comes back, reads the successor out of its account
/// store, and asks.
///
/// **The verdict is the chain's, never the nest's.** The lookup is walked and
/// every hop verified against the registration chain exactly as
/// [`resolve_successor`] does, and the match is on the successor **we hold the
/// key for** — so a nest that invents a succession cannot make us adopt one, and
/// a nest that hides ours can only leave us in `NotLanded`, which is the
/// conservative side (the seed stays persisted either way). Supply `known_head`
/// when the caller has one, for the same rewrite/truncation guard
/// [`resolve_successor`] documents.
pub async fn reconcile_succession<R>(
    client: &RecoveryClient<R>,
    old_actor_id: ActorId,
    successor: &ActorKeypair,
    known_head: Option<ChainHead>,
) -> Result<ReconciledSuccession>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let walked = walk_succession_path(client, old_actor_id, known_head).await?;
    let Some((terminal, _)) = walked.last() else {
        return Ok(ReconciledSuccession::NotLanded);
    };
    let terminal_actor_id = terminal.new_actor_id;
    let ours = successor.actor_id();

    // Search every hop, not just the terminal one: our succession landing and
    // then being succeeded again is a real (if slow) sequence, and in it the
    // account still passed through the key we hold — the caller's sweep is over
    // *our* hop's statement, which is the pair its two engines carry.
    for (step, statement) in walked {
        if step.new_actor_id == ours {
            return Ok(ReconciledSuccession::Landed(Box::new(SuccessionHandoff {
                successor_secret_hex: zeroize::Zeroizing::new(hex32(successor.secret_bytes())),
                new_actor_id: ours,
                // Not knowable here — see the field's doc.
                succeeded_at: None,
                statement,
            })));
        }
    }
    Ok(ReconciledSuccession::LandedForAnother {
        new_actor_id: terminal_actor_id,
    })
}
