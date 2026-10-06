//! The **pull** half of identity-succession propagation — and, since the
//! unanchored-verify hardening, the **only** verification path a peer runs
//! (`docs/goal/behavior/identity-succession.md` § Propagation → *Federation
//! peers*, "peers can pull the chain from the old identity's home").
//!
//! The push leg is best-effort by construction: a peer that was down, or whose
//! address this nest never recorded, simply does not learn that an identity it
//! holds residue about was superseded — and until it does, a stolen key's fresh
//! content keeps arriving here and being accepted. `identity-succession.md:103`
//! declares that residual and names push **and pull** as what shrinks it, so
//! this worker asks rather than waits.
//!
//! ## The anchor rule (why a push is a hint, never evidence)
//!
//! The adversary of the succession plane holds the victim's identity seed
//! (`identity-succession.md:28`), and a seed holder can mint a registration
//! chain every signature of which is genuine — so "the chain verifies" proves
//! seed possession, not RecoveryKey legitimacy, and bytes delivered *by the
//! party asserting the succession* can never be the trust anchor. What a peer
//! may trust instead is exactly what `identity-succession.md:55` grants a
//! consumer: the chain **it fetches from the home nest it already knew** for
//! the identity. Concretely, both legs resolve the anchor through
//! [`resolve_anchor`] — the nest **identity** of the *oldest residue row
//! overall* ([`crate::db::CacheDb::oldest_foreign_member_nest_id`]) at first
//! contact, and the **persisted** [`crate::db::CacheDb::foreign_recovery_head`]
//! `anchor_nest_id` pin once the identity has ever been verified, which a later
//! (attacker-mintable) binding never displaces. **The dial then PROVES that
//! identity** ([`fauna_anon_client::AnonymousNestClient::verify_nest_identity`])
//! before a byte is trusted, so a URL resolved from an attacker-influenceable
//! binding fails closed rather than becoming the anchor. The persisted chain
//! head this nest learned is passed as `known`, so a served chain must *extend*
//! it rather than replace it. An identity with no addressable anchor is refused,
//! not TOFU'd from pushed bytes: the un-addressable-peer residual
//! `identity-succession.md:104` already declares.
//!
//! A `fauna.federation.succession.push` therefore only *wakes* this path:
//! the handler checks the hint is plausible (not local, not already known,
//! anchored, not throttled) and then runs [`verify_and_record_from_home`]
//! against this nest's own recorded anchor — never against the pushed chain.
//!
//! ## Why this is not a `fauna.federation.*` kind
//!
//! Every *residue-surface* call rides the nest↔nest channel as its sole carrier
//! (`federation.md` § Federation residue surface) — but that surface is the set
//! of authenticated cross-nest calls. The two kinds this worker reads,
//! `fauna.recovery.succession.lookup` and `fauna.recovery.registration.chain`,
//! are **pre-identity public directory reads**: the anchor rule above, not the
//! transport, is what makes their replies trustworthy. The nest already dials
//! peers anonymously for exactly this reason
//! (`federation_pool::resolve_peer_nest_id` → `fauna.nest.info`).
//!
//! ## What it will not do
//!
//! Nothing here extends trust beyond the anchor. The anchored home nest is
//! trusted at exactly the TOFU-plus-known-head grade the goal doc assigns:
//! a first fetch is believed (first contact), every later fetch must extend
//! the persisted head. The known head bounds a hostile *chain-server*, but not
//! the seed thief that IS the plane's adversary: a thief holding the seed can
//! extend the genuine chain with a seed-alone link that *visits* the known head
//! (the legitimate honest-loss arm), so the head alone does not refuse a
//! takeover — only the anchor-**identity** proof does, by refusing a box that
//! cannot sign as the pinned home. A hostile answering nest's best outcome is to
//! answer nothing, indistinguishable from the honest "never succeeded" reply —
//! leaving this nest exactly where it started.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fauna_core::recovery::ChainHead;
use fauna_protocol::recovery::{
    RegistrationChainReply, RegistrationChainRequest, SuccessionLookupReply,
    SuccessionLookupRequest,
};
use serde_bytes::ByteBuf;

use crate::routes::AppState;

/// Reconcile cadence — hard-coded (bucket 1: no human chooses it).
///
/// Hourly. The push leg is what makes propagation *prompt*; this is the backstop
/// for peers push could not reach, where the cost of an hour's latency is bounded
/// by the same refusal the home nest already applies at the source.
const PULL_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Most remote identities reconciled per pass.
///
/// Each one costs a dial to a peer that may be slow or hostile, so the pass is
/// bounded rather than proportional to residue. Un-visited identities are simply
/// picked up next hour — the work list is re-read every pass and skips whatever
/// is already known, so a large residue set drains across passes instead of
/// making one pass unbounded.
const MAX_PULLS_PER_PASS: usize = 64;

/// Most candidate anchor URLs tried for one identity before giving up on it
/// this pass (bucket 1).
///
/// Residue is nest-wide and any User-class caller can add a binding, so the
/// candidate list is attacker-extendable and the walk over it must be bounded.
/// It is *not* a bound on correctness: candidates are ordered **proven-first,
/// then oldest-first** ([`crate::db::CacheDb::resolve_foreign_nest_urls`]), and
/// an attacker cannot reach proven status without the honest nest's key, so an
/// address this nest has actually dialed stays ahead of anything a caller merely
/// asserted. Every dial spent here also counts against [`MAX_PULLS_PER_PASS`],
/// so one crowded identity cannot make a pass unbounded — it can only spend its
/// own share of it.
///
/// (An earlier revision justified the bound with *"an attacker can only add rows
/// later"*. That was false while the address lived on the membership row, whose
/// upsert let a plant rewrite an existing row in place. The
/// `nest_addresses` directory makes addresses join rather than displace, which
/// is what restores the property this bound leans on.)
const MAX_ANCHOR_CANDIDATES: usize = 8;

/// Per-identity cooldown on push-hint-triggered dials (bucket 1).
///
/// A push is unauthenticated-in-content, so it must not be a lever for making
/// this nest dial on demand: after one hint-triggered check of an identity,
/// further hints for it are ignored for this long. Succession is a rare, human
/// ceremony and the hourly pull is the backstop, so the honest cost of the
/// cooldown is bounded by [`PULL_INTERVAL`] anyway.
const SUCCESSION_HINT_COOLDOWN: Duration = Duration::from_secs(600);

/// Ceiling on one push-hint's whole verify (dial + fetch + verify), so a
/// hint about an unresponsive anchor bounds the handler instead of hanging it
/// (bucket 1).
const HINT_DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Most hint-triggered dials in flight at once (bucket 1). Beyond this, hints
/// are dropped — the hourly pull covers whatever a burst crowded out.
const MAX_CONCURRENT_HINT_DIALS: usize = 8;

/// Throttle state for push-hint-triggered verifies — per-[`AppState`], since
/// several in-process nests must not share a cooldown table.
pub struct SuccessionHintState {
    cooldown: tokio::sync::Mutex<HashMap<[u8; 32], Instant>>,
    dials: tokio::sync::Semaphore,
}

impl Default for SuccessionHintState {
    fn default() -> Self {
        Self {
            cooldown: tokio::sync::Mutex::new(HashMap::new()),
            dials: tokio::sync::Semaphore::new(MAX_CONCURRENT_HINT_DIALS),
        }
    }
}

impl SuccessionHintState {
    /// Admit one hint-triggered verify for `actor`, or say why not.
    ///
    /// On admission the actor's cooldown starts immediately — a failed dial is
    /// still a spent dial, which is the DoS accounting that matters.
    pub async fn try_admit(&self, actor: &[u8; 32]) -> Option<tokio::sync::SemaphorePermit<'_>> {
        {
            let mut cooldown = self.cooldown.lock().await;
            let now = Instant::now();
            // Opportunistic sweep so the map tracks live cooldowns, not history.
            cooldown.retain(|_, at| now.duration_since(*at) < SUCCESSION_HINT_COOLDOWN);
            if cooldown.contains_key(actor) {
                return None;
            }
            cooldown.insert(*actor, now);
        }
        self.dials.try_acquire().ok()
    }
}

/// Spawn the periodic succession pull via the shared
/// [`crate::sweeper::spawn_periodic_sweeper`] primitive: a bare interval loop
/// whose first tick fires immediately (the at-boot reconcile — the moment a
/// nest that was *down* during a peer's push catches up), torn down at process
/// exit.
pub fn spawn_succession_pull(state: Arc<AppState>) {
    let scope = state.clone();
    scope.scope_handle(crate::sweeper::spawn_periodic_sweeper(
        PULL_INTERVAL,
        false,
        move || {
            let state = state.clone();
            async move {
                match pull_successions_once(&state).await {
                    Ok(0) => {}
                    Ok(n) => {
                        tracing::info!(target: "recovery", "succession pull: learned {n} link(s)")
                    }
                    Err(e) => tracing::error!(target: "recovery", "succession pull: {e}"),
                }
            }
        },
    ));
}

/// One reconcile pass. Returns how many succession links this nest learned.
///
/// `pub` so a test can drive a single deterministic pass instead of waiting on
/// the interval — the cadence is not what any assertion is about.
pub async fn pull_successions_once(state: &Arc<AppState>) -> anyhow::Result<usize> {
    let actors = state.db.distinct_foreign_member_actors().await?;
    if actors.is_empty() {
        return Ok(0);
    }

    let mut learned = 0usize;
    let mut visited = 0usize;
    for actor in actors {
        // Already known: `old_actor_id` is a PRIMARY KEY, so a second link can
        // never land anyway — skipping keeps the pass proportional to what is
        // actually unresolved.
        if state.db.succession_for(&actor[..]).await?.is_some() {
            continue;
        }
        // Resolve the anchor: the identity this nest trusts as the chain source
        // (the persisted pin, else the oldest-row identity for first contact),
        // and every URL that names it. No addressable binding for that identity ⇒
        // the un-addressable residual — refuse, never dial another identity's
        // URL (`identity-succession.md:104`).
        let Some((expected_id, candidates)) = resolve_anchor(state, &actor).await? else {
            continue;
        };
        // Walk the candidates until one *proves* the anchor identity. A failed
        // dial must not end the actor's pass: residue is nest-wide and any
        // User-class caller can add a binding, so a single unreachable row —
        // planted, or just a stale address after a genuine migration — would
        // otherwise deny this identity's succession on every future
        // pass. Each attempt spends the pass budget, so the walk
        // is bounded twice over.
        for home_url in candidates {
            if visited >= MAX_PULLS_PER_PASS {
                tracing::debug!(
                    target: "recovery",
                    "succession pull hit the per-pass bound; remainder next pass"
                );
                return Ok(learned);
            }
            visited += 1;
            match verify_and_record_from_home(state, &actor, &expected_id, &home_url).await {
                // The anchor identity was proven, so this box is the authority
                // for the identity and its answer is final for this pass —
                // whether or not there was anything to learn. Trying further
                // candidates could only re-ask a question already answered.
                Ok(n) => {
                    learned += n;
                    break;
                }
                // A peer that is down, slow, or lying is the ordinary case this
                // worker exists to survive; it is not an error for the pass.
                // Try the next address that claims the same identity.
                Err(e) => tracing::debug!(
                    target: "recovery",
                    peer = %home_url,
                    "succession pull from peer failed: {e}"
                ),
            }
        }
    }
    Ok(learned)
}

/// Resolve `actor`'s anchor: the nest **identity** this nest will trust as the
/// chain source, plus every URL that names it. The identity is the persisted
/// [`crate::db::CacheDb::foreign_recovery_head`] `anchor_nest_id` pin once this
/// nest has ever verified the identity — so a later re-invite that rewrites the
/// oldest residue binding cannot move a proven anchor — else the oldest residue
/// row's `home_nest_id`, the binding learned first (first-contact TOFU, the
/// grade `identity-succession.md:63` grants). `None` when the identity has no
/// residue at all, or no addressable binding names it: the un-addressable
/// residual, where both propagation legs refuse rather than fall back to bytes a
/// pusher delivered or to another identity's URL.
///
/// The URLs are **candidates**, oldest-first and capped at
/// [`MAX_ANCHOR_CANDIDATES`] — the caller dials them in order until one proves
/// the identity. Returning only one made a planted binding a permanent denial
/// of this identity's succession; the identity half is
/// unchanged, since it is the pin that decides *whom* to trust and this list
/// only decides *where* to look for them.
async fn resolve_anchor(
    state: &Arc<AppState>,
    actor: &[u8; 32],
) -> anyhow::Result<Option<([u8; 32], Vec<String>)>> {
    let pinned = state
        .db
        .foreign_recovery_head(&actor[..])
        .await?
        .map(|h| h.anchor_nest_id);
    let expected = match pinned {
        Some(id) => id,
        None => match state.db.oldest_foreign_member_nest_id(actor).await? {
            Some(id) => id,
            None => return Ok(None),
        },
    };
    let candidates = state
        .db
        .resolve_foreign_nest_urls(&expected, MAX_ANCHOR_CANDIDATES)
        .await?;
    if candidates.is_empty() {
        return Ok(None);
    }
    Ok(Some((expected, candidates)))
}

/// Ask `actor`'s **anchored** home nest about it and record what verifies.
/// Returns how many links were learned (0 or 1).
///
/// This is the one verification path both propagation legs share: the hourly
/// pull calls it with the anchor URL from the work list, and a push hint calls
/// it (throttled, bounded) with the same anchor freshly read — never with
/// anything the pusher supplied.
///
/// **One hop per call, deliberately.** `succession.lookup` returns the whole
/// `old → terminal` path, but only the *first* hop's old identity is the one
/// this call holds an anchor for. Recording that hop re-points the residue to
/// the successor — inheriting the binding rows and their URL — so the next
/// pass (or hint) anchors the next hop the same way, instead of this call
/// trusting the dialed nest for identities it has no anchor about.
///
/// Chain verification always passes the persisted
/// [`crate::db::CacheDb::foreign_recovery_head`] as `known`, and re-persists
/// the head a verified chain establishes — including when no succession exists
/// yet (the opportunistic learn): that is what shrinks first-contact TOFU to
/// the one fetch that happens before an identity was ever verified here.
pub async fn verify_and_record_from_home(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    expected_nest_id: &[u8; 32],
    home_url: &str,
) -> anyhow::Result<usize> {
    let client = fauna_anon_client::AnonymousNestClient::connect(home_url)
        .await
        .map_err(|e| anyhow::anyhow!("anon connect: {e}"))?;

    // Prove the endpoint holds the anchor identity BEFORE trusting a byte it
    // serves. A box reached by an attacker-influenceable URL that cannot sign as
    // the pinned identity is refused here — the takeover the anchor-plantability
    // hardening closes. Fail closed: any verification failure
    // teaches this pass nothing.
    client
        .verify_nest_identity(home_url, *expected_nest_id)
        .await
        .map_err(|e| anyhow::anyhow!("anchor identity proof: {e}"))?;

    let known = state
        .db
        .foreign_recovery_head(&actor[..])
        .await?
        .map(|h| ChainHead::new(h.recovery_pubkey, h.seq));

    let lookup: SuccessionLookupReply = client
        .request(
            "fauna.recovery.succession.lookup",
            SuccessionLookupRequest {
                actor_id: ByteBuf::from(actor.to_vec()),
                extra: Default::default(),
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("succession.lookup: {e}"))?;

    let fetch_chain = || async {
        let reply: RegistrationChainReply = client
            .request(
                "fauna.recovery.registration.chain",
                RegistrationChainRequest {
                    actor_id: ByteBuf::from(actor.to_vec()),
                    extra: Default::default(),
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!("registration.chain: {e}"))?;
        let decoded = reply
            .registrations
            .iter()
            .filter_map(|r| {
                fauna_core::encoding::canonical_decode::<
                    fauna_core::recovery::SignedRecoveryKeyRegistration,
                >(r.as_ref())
                .ok()
            })
            .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>(decoded)
    };

    let Some(bytes) = lookup.statements.into_iter().next() else {
        // No succession — the common case. Learn (or advance) the chain head
        // while the dial is warm, so a later succession must extend what we
        // saw today rather than getting first-contact grade.
        if known.is_none() {
            let chain = fetch_chain().await?;
            if !chain.is_empty()
                && let Ok(head) = fauna_core::recovery::verify_registration_chain(
                    fauna_core::identity::ActorId(*actor),
                    &chain,
                    None,
                )
            {
                state
                    .db
                    .record_foreign_recovery_head(
                        &actor[..],
                        &head.recovery_pubkey,
                        head.seq,
                        expected_nest_id,
                    )
                    .await?;
                tracing::debug!(target: "recovery", "learned a recovery chain head");
            }
        }
        return Ok(0);
    };

    let signed: fauna_core::recovery::SignedIdentitySuccession =
        match fauna_core::encoding::canonical_decode(bytes.as_ref()) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(target: "recovery", "undecodable statement from peer: {e}");
                return Ok(0);
            }
        };
    // The lookup is keyed by this actor; a reply leading with a different
    // identity is not the hop we hold an anchor for.
    if signed.statement.old_actor_id.0 != *actor {
        tracing::debug!(target: "recovery", "lookup reply names a different identity; ignoring");
        return Ok(0);
    }

    let chain = fetch_chain().await?;
    let verified = match fauna_core::recovery::verify_succession_against_chain(
        &signed,
        &chain,
        known.as_ref(),
    ) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "recovery",
                peer = %home_url,
                error = %e,
                "pulled succession statement refused"
            );
            return Ok(0);
        }
    };
    // The chain verified against everything we knew — persist its head before
    // anything else, so even a refused statement can never un-learn it.
    state
        .db
        .record_foreign_recovery_head(
            &actor[..],
            &verified.chain_head.recovery_pubkey,
            verified.chain_head.seq,
            expected_nest_id,
        )
        .await?;

    match state
        .db
        .record_peer_succession(
            &verified.old_actor_id.0[..],
            &verified.new_actor_id.0[..],
            bytes.as_ref(),
            verified.seq,
        )
        .await?
    {
        Ok(applied) => {
            tracing::info!(
                target: "recovery",
                peer = %home_url,
                seq = verified.seq,
                foreign_memberships = applied.foreign_memberships,
                member_grants = applied.member_grants,
                contact_edges = applied.contact_edges,
                "learned a succession from the anchored home nest"
            );
            Ok(1)
        }
        // Already known, or this nest is the identity's own home — both mean
        // "nothing to learn here".
        // An identity that signed two successions is a state the honest
        // ceremony cannot produce (ruling (8)(j)(1)): warn, naming the successor.
        Err(refusal @ crate::db::successions::SuccessionRefusal::NewAlreadySucceeded) => {
            tracing::warn!(
                target: "recovery",
                successor = %hex::encode(verified.new_actor_id.0),
                %refusal,
                "pulled succession refused"
            );
            Ok(0)
        }
        Err(refusal) => {
            tracing::debug!(target: "recovery", %refusal, "pulled succession not recorded");
            Ok(0)
        }
    }
}

/// Run one push-hint-triggered verify for `actor`, throttled and bounded.
/// Returns whether a link was learned.
///
/// The push handler calls this after its own fast refusals; everything the
/// pusher sent beyond the identity being named is already forgotten by the
/// time this runs.
pub async fn verify_from_hint(state: &Arc<AppState>, actor: &[u8; 32]) -> bool {
    let Ok(Some((expected_id, candidates))) = resolve_anchor(state, actor).await else {
        // No addressable anchor: nothing this nest may trust for the identity
        // (`identity-succession.md:104`'s declared residual). Refuse the hint.
        return false;
    };
    let Some(_permit) = state.succession_hints.try_admit(actor).await else {
        tracing::debug!(target: "recovery", "succession push hint throttled");
        return false;
    };
    // The candidate walk runs INSIDE the one ceiling, not once per candidate:
    // a hint must stay bounded by [`HINT_DIAL_TIMEOUT`] however many addresses
    // claim the identity, or the fix for would hand a pusher an
    // N-times-longer handler.
    let walk = async {
        for url in &candidates {
            match verify_and_record_from_home(state, actor, &expected_id, url).await {
                // Anchor proven — final answer for this hint (see the pull walk).
                Ok(n) => return n > 0,
                Err(e) => {
                    tracing::debug!(
                        target: "recovery",
                        peer = %url,
                        "hint-triggered verify failed: {e}"
                    );
                }
            }
        }
        false
    };
    match tokio::time::timeout(HINT_DIAL_TIMEOUT, walk).await {
        Ok(learned) => learned,
        Err(_) => {
            tracing::debug!(target: "recovery", "hint-triggered verify timed out");
            false
        }
    }
}
