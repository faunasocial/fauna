//! Transport-agnostic payment-entitlement engine — Pillar 3 of
//! `docs/goal/behavior/monetization.md`.
//!
//! Consumes the [`fauna_payments`] narrow waist: a verified payment event
//! (already authenticated by a provider adapter) becomes a tier entitlement
//! through Pillar 1's existing machinery — the third grant source next to
//! manual approval and tier `auto_approve`:
//!
//! - **Bound buyer** (`client_reference_id = actor_id`, or a redeemed claim):
//!   every tier's period key is client-minted, so the grant enqueues a
//!   `payment_entitled` subscribe request that the creator's client drain
//!   pump approves on its next pass (the same queue subscribe uses — the
//!   client mints the KeyBlob).
//! - **Unbound payment**: mint a post-payment claim code (idempotent per
//!   provider `external_ref` — providers redeliver webhooks).
//! - **Refund/dispute**: void the paid window (`valid_until`) wherever the
//!   entitlement currently lives (active subscriber row, pending request
//!   marker, unredeemed claim). No revocation list anywhere.
//!
//! Mirrors the `invite_core` split: this module is `fauna_protocol`-free; the
//! WS-RPC handler layer (`payment_handlers`) and the HTTP webhook ingress
//! (`payment_routes`) map [`GrantError`] / [`RedeemError`] onto their own
//! error vocabularies.

use std::sync::Arc;

use fauna_payments::{Buyer, PaymentEntitlement};

use crate::db::now_epoch_secs;
use crate::routes::AppState;

/// What applying a payment did.
#[derive(Debug, PartialEq, Eq)]
pub enum PaymentApplied {
    /// The buyer was bound, so the entitlement was applied directly.
    /// `queued = true` means it was enqueued for the payee's client to mint
    /// (every content tier); `false` means it is active now (a renewal of an
    /// already-active subscriber, or a membership grant).
    Granted { queued: bool },
    /// The payment carried no usable buyer binding, so a claim code was minted
    /// for the buyer to redeem later.
    ClaimMinted { code: String },
    /// Same, but this exact payment was already seen (mechanisms redeliver) —
    /// the previously minted code, returned idempotently.
    ClaimExists { code: String },
}

/// Convert the waist's provider-native `u64` window to the engine's signed
/// epoch seconds, saturating rather than wrapping — the value originates
/// outside the box, and a wrapped negative `valid_until` would read as
/// "expired long ago" at every entitlement gate.
fn valid_until_of(entitlement: &PaymentEntitlement) -> Option<i64> {
    entitlement
        .valid_until_secs
        .map(|s| i64::try_from(s).unwrap_or(i64::MAX))
}

/// Why a grant could not be applied.
#[derive(Debug)]
pub enum GrantError {
    /// The mapped tier no longer exists (provider config dangles after a
    /// tier delete) — surfaced non-2xx so the creator notices via the
    /// provider dashboard.
    TierNotFound,
    Internal(String),
}

impl std::fmt::Display for GrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrantError::TierNotFound => write!(f, "mapped tier not found"),
            GrantError::Internal(e) => write!(f, "internal: {e}"),
        }
    }
}

fn internal(e: impl std::fmt::Display) -> GrantError {
    GrantError::Internal(format!("{e}"))
}

/// Apply a verified payment — **the engine's one entry point for every
/// payment mechanism**, and the reason the waist exists.
///
/// A mechanism's whole job is to produce a [`PaymentEntitlement`]: webhook
/// ingress verifies a provider signature and normalizes the event
/// ([`PaymentEntitlement::from_verified_event`]); claim redemption reads the
/// stored claim row; a future zap adapter will validate a receipt. From here
/// down nothing is mechanism-aware — and nothing is *target*-aware either: the
/// tier's own designation decides whether this entitlement unlocks content
/// keys or nest membership (monetization.md § *One model, many mechanisms, two
/// targets*).
pub async fn apply_payment(
    state: &Arc<AppState>,
    entitlement: &PaymentEntitlement,
) -> Result<PaymentApplied, GrantError> {
    let payee = &entitlement.payee.0;
    let valid_until = valid_until_of(entitlement);

    match &entitlement.buyer {
        Buyer::Actor(buyer) => {
            let queued =
                grant_paid_entitlement(state, payee, &buyer.0, &entitlement.tier, valid_until)
                    .await?;
            Ok(PaymentApplied::Granted { queued })
        }
        Buyer::Unbound => mint_claim(state, entitlement, valid_until).await,
    }
}

/// The `payments.unlock.purchase` [`GateOp`] for a waist entitlement — the
/// **newness-delta resolution** for the buy side's second surface
/// (`dynamic-features.md` § The quota grammar's third refinement).
///
/// It lives here, beside the waist, because "is this counterparty new?" is a
/// question about *the payments plane's own records* — the payee's live
/// subscriber rows — and every mechanism must resolve it the same way or the
/// dimension means different things on different doors. It deliberately stops
/// there: it returns the op and performs no gate, so each transport keeps
/// mapping refusals onto its own error vocabulary (the `GrantError` split this
/// module's header states), and this file stays `fauna_protocol`-free.
///
/// **The count is never read back from these rows.** They decide only whether
/// this operation introduces someone; the bucket keeps what it was told, so an
/// unsubscribe cannot refund the unit it already spent — the monotonicity that
/// makes the counterparty dimension a structural bound rather than a
/// user-mutable one.
///
/// `magnitude_msats` is the mechanism's own value when it carries a
/// machine-comparable one (a zap receipt's amount) and `0` when it does not (a
/// provider webhook names a tier, never a price this nest can compare — Fauna
/// never parses provider-side prices). `0` is honest here, not unset.
pub async fn purchase_gate_op(
    state: &Arc<AppState>,
    entitlement: &PaymentEntitlement,
    magnitude_msats: u64,
) -> Result<fauna_core::feature_gate::GateOp, GrantError> {
    let payee = &entitlement.payee.0;
    let new_counterparties = match &entitlement.buyer {
        Buyer::Actor(buyer) => {
            let standing = !state
                .db
                .get_subscribed_tiers(payee, &buyer.0)
                .await
                .map_err(internal)?
                .is_empty();
            u64::from(!standing)
        }
        // An unbound payment names a buyer this box cannot identify, and one it
        // has never been able to identify — so it cannot be a *standing*
        // counterparty by construction, and counting it as new is both the
        // truthful answer and the fail-tight one. (The claim code it mints binds
        // a buyer later; that redemption spends on the redeemer's own account at
        // `payments.claim.redeem`, never again on the payee's.)
        Buyer::Unbound => 1,
    };
    Ok(fauna_core::feature_gate::GateOp {
        feature: fauna_core::feature_gate::GatedFeature::Payments,
        surface: fauna_core::feature_gate::SURFACE_PAYMENTS_UNLOCK_PURCHASE,
        new_counterparties,
        magnitude: magnitude_msats,
    })
}

/// Mint (or idempotently return) the claim code for an unbound payment — the
/// universal buyer-binding fallback (monetization.md § Pillar 3 Q4). Lives in
/// the engine, not in any one mechanism's transport module, so every mechanism
/// gets the same idempotency and the same audit trail.
async fn mint_claim(
    state: &Arc<AppState>,
    entitlement: &PaymentEntitlement,
    valid_until: Option<i64>,
) -> Result<PaymentApplied, GrantError> {
    let payee = &entitlement.payee.0;

    // Idempotency: a redelivered payment returns the already-minted code.
    if let Some(existing) = state
        .db
        .find_payment_claim_by_external_ref(payee, &entitlement.provider, &entitlement.external_ref)
        .await
        .map_err(internal)?
    {
        return Ok(PaymentApplied::ClaimExists {
            code: existing.code,
        });
    }

    // The tier must exist — and be subscribable — for the claim to be
    // redeemable later. A hidden tier (ruling 4, `monetization.md` § The
    // unifying model → *A tier may be hidden*) answers the same `TierNotFound`
    // here as `grant_paid_entitlement` does, so a claim naming one is never
    // minted only to be burned unredeemed.
    if !state
        .db
        .get_subscription_tier(payee, &entitlement.tier)
        .await
        .map_err(internal)?
        .is_some_and(|t| !t.hidden)
    {
        return Err(GrantError::TierNotFound);
    }

    // Same retry contract as invite codes: a rare code collision surfaces as
    // an insert error and the next attempt re-mints.
    for _ in 0..3 {
        let code = crate::admin::generate_invite_code();
        match state
            .db
            .insert_payment_claim(
                &code,
                payee,
                &entitlement.tier,
                &entitlement.provider,
                &entitlement.external_ref,
                valid_until,
            )
            .await
        {
            Ok(()) => return Ok(PaymentApplied::ClaimMinted { code }),
            Err(e) => tracing::warn!("payment claim insert retry: {e}"),
        }
    }
    Err(GrantError::Internal("claim code mint failed".into()))
}

/// Grant a verified paid entitlement to a bound buyer. Returns `queued`:
/// `false` = the subscription is active now (a window extension on an
/// already-active subscriber); `true` = enqueued for the creator's client to
/// mint.
///
/// Private on purpose: mechanisms reach the engine through
/// [`apply_payment`]'s waist value, never by assembling loose scalars.
async fn grant_paid_entitlement(
    state: &Arc<AppState>,
    author_id: &[u8; 32],
    buyer_id: &[u8; 32],
    tier_name: &str,
    valid_until: Option<i64>,
) -> Result<bool, GrantError> {
    let tier = state
        .db
        .get_subscription_tier(author_id, tier_name)
        .await
        .map_err(internal)?
        .ok_or(GrantError::TierNotFound)?;

    // Ruling 4 (`monetization.md` § The unifying model → *A tier may be
    // hidden*): hidden means not offered AND not subscribable — and the buy
    // side is a subscribe door too. Without this, a verified payment naming a
    // hidden tier would land a `payment_entitled` row, which the author's drain
    // pump approves WITHOUT judgment — minting the reserved owner-only tier's
    // period key to the payer, exactly the accidental grant the `subscribe`
    // refusal exists to prevent. Same value a nonexistent tier gets, so the
    // buy side leaks no more than the subscribe side does.
    if tier.hidden {
        return Err(GrantError::TierNotFound);
    }

    // A MEMBERSHIP tier (monetization.md § Pillar 4) gates NEST MEMBERSHIP, not
    // content keys — the target axis lives on the tier's designation, so the same
    // waist value routes here instead of the content-entitlement path below.
    // A designation whose subscription tier was since deleted is inert: the
    // `get_subscription_tier` above already returned `TierNotFound`, so this read
    // is never reached for one (the step-(2) "no FK to subscription_tiers"
    // rationale made real).
    if let Some(designation) = state
        .db
        .get_membership_tier(author_id, tier_name)
        .await
        .map_err(internal)?
    {
        return grant_membership(
            state,
            author_id,
            buyer_id,
            tier_name,
            &designation,
            valid_until,
        )
        .await;
    }

    // Already entitled (expiry-aware): a renewal — extend/refresh the window
    // in place. No re-approve, no queue round-trip.
    if state
        .db
        .is_subscriber(author_id, buyer_id, tier_name)
        .await
        .map_err(internal)?
    {
        state
            .db
            .set_subscriber_valid_until(author_id, buyer_id, tier_name, valid_until)
            .await
            .map_err(internal)?;
        return Ok(false);
    }

    // The nest holds no key material: enqueue exactly like a
    // subscriber-initiated request, marked payment-entitled so the author's
    // drain pump approves it without creator judgment.
    state
        .db
        .upsert_payment_entitled_request(author_id, buyer_id, tier_name, valid_until)
        .await
        .map_err(internal)?;
    // Grant-time rank fan-out (`monetization.md:126`): enqueue the
    // included `unlocks_post`-designated tiers alongside the paid tier's
    // own request, so ONE author-client drain pass delivers everything
    // (no-op when the paid tier is itself designated — one cheap post
    // must never unlock every sold post). Best-effort: the paid enqueue
    // above is the grant and has landed; a missed fan-out heals at the
    // next boot reconcile or the buyer's idempotent re-subscribe.
    if let Err(e) = state
        .db
        .enqueue_unlock_fanout(author_id, buyer_id, tier_name)
        .await
    {
        tracing::error!("payment grant: unlock fan-out enqueue({tier_name}): {e}");
    }
    Ok(true)
}

/// Grant a verified **membership** entitlement (monetization.md § Pillar 4
/// Rail C): the entitlement gates nest membership, so it admits/assigns the
/// buyer at the linked quota tier and records the membership subscription
/// directly. Never enqueues for a client to mint — admission needs no author
/// key material (any members-only *content* the admin also gates to this tier
/// catches up on the author's next rotation, the accepted honest-box latency
/// bound). Returns `queued = false`: the membership is active now.
async fn grant_membership(
    state: &Arc<AppState>,
    admin_id: &[u8; 32],
    buyer_id: &[u8; 32],
    tier_name: &str,
    designation: &crate::db::MembershipTierRow,
    valid_until: Option<i64>,
) -> Result<bool, GrantError> {
    let admin_tier = designation.admin_tier.as_str();

    // Nest-membership assignment. Three cases, mutually exclusive in practice
    // (a pending requester has no account; a registered actor has no pending
    // request):
    //   1. a PENDING invite request → auto-approve it without admin judgment
    //      (the `payment_entitled` analog, § Pillar 4 Rail C step 1) — create
    //      the account at the linked quota tier;
    //   2. an already-registered actor → assign `users.tier = admin_tier`
    //      (an existing member buying or renewing — § Rail C step 2);
    //   3. neither → record the subscription only; the payment binds a buyer
    //      with no nest presence yet (a denied request, or a pre-emptive buy).
    if let Some(req) = state
        .db
        .get_invite_request_by_actor(buyer_id)
        .await
        .map_err(internal)?
        .filter(|r| r.status == "pending")
    {
        admit_pending_invite_request(state, buyer_id, admin_tier, &req).await?;
    } else if state
        .db
        .is_actor_registered(buyer_id)
        .await
        .map_err(internal)?
    {
        state
            .db
            .set_user_tier(buyer_id, admin_tier)
            .await
            .map_err(internal)?;
    }

    // Record the membership subscription: a `subscribers` row under the admin's
    // tier carrying `valid_until` (§ Pillar 4 Rail C step 1). `add_subscriber`
    // resets `valid_until` to NULL on conflict, so stamp the window right after
    // (the same order `requests.approve` uses for a paid request).
    state
        .db
        .add_subscriber(admin_id, buyer_id, tier_name, None)
        .await
        .map_err(internal)?;
    state
        .db
        .set_subscriber_valid_until(admin_id, buyer_id, tier_name, valid_until)
        .await
        .map_err(internal)?;
    // Freeze the quota pair this admission/renewal ran under (§ Rail C step 3).
    // Re-stamped on every renewal, so a member who renews after the admin
    // re-points the link moves onto the new policy — the designation governs
    // what you buy *now*, and only an already-bought window is held to the terms
    // it was bought under.
    state
        .db
        .stamp_membership_admission(
            admin_id,
            buyer_id,
            tier_name,
            admin_tier,
            &designation.lapse_tier,
        )
        .await
        .map_err(internal)?;
    Ok(false)
}

/// Create the account a payment-backed pending invite request describes —
/// mirroring the `fauna.admin.invite_requests.approve` handler's admission, but
/// with the verified payment as the authorization instead of admin judgment.
/// The handle is re-validated at admission time (a pending row can predate a
/// reserved-list fix, the same boundary the approve handler enforces). A handle
/// that is now taken or invalid leaves the request pending for manual admin
/// handling rather than failing the whole payment — the caller still records the
/// entitlement, so a rare collision degrades to manual approval, never a lost
/// payment or a webhook-retry loop.
async fn admit_pending_invite_request(
    state: &Arc<AppState>,
    buyer_id: &[u8; 32],
    admin_tier: &str,
    req: &crate::db::InviteRequestRow,
) -> Result<(), GrantError> {
    if crate::registration::validate_handle(&req.handle).is_err()
        || state
            .auth
            .registration
            .reserved_handles
            .iter()
            .any(|r| r == &req.handle)
    {
        tracing::warn!(
            "membership payment: pending-request handle failed re-validation; left pending"
        );
        return Ok(());
    }
    if state
        .db
        .resolve_handle(&req.handle)
        .await
        .map_err(internal)?
        .is_some()
    {
        tracing::warn!("membership payment: pending-request handle now taken; left pending");
        return Ok(());
    }
    if let Err(e) = state
        .db
        .create_user_with_handle(buyer_id, admin_tier, &req.handle, None)
        .await
    {
        tracing::error!("membership payment: create_user_with_handle failed: {e:?}; left pending");
        return Ok(());
    }
    let _ = state.db.delete_invite_request(req.id).await;
    Ok(())
}

/// Apply a refund/dispute: void the paid window wherever the entitlement
/// currently lives. Returns `true` if anything was voided (`false` = the
/// event was authentic but matched nothing — e.g. a refund for an unknown
/// payment; acknowledged, no action).
///
/// Takes the same waist value [`apply_payment`] does — a refund names exactly
/// the entitlement it voids, so the mechanism produces one shape for both
/// verbs.
pub async fn apply_refund(
    state: &Arc<AppState>,
    entitlement: &PaymentEntitlement,
) -> Result<bool, GrantError> {
    let author_id = &entitlement.payee.0;
    let provider_kind = entitlement.provider.as_str();
    let tier_name = entitlement.tier.as_str();
    let external_ref = entitlement.external_ref.as_str();
    let buyer_id = match &entitlement.buyer {
        Buyer::Actor(a) => Some(a.0),
        Buyer::Unbound => None,
    };

    let mut acted = false;
    let now = now_epoch_secs();

    // A claim minted for this payment: void it if unredeemed; if already
    // redeemed, fall through to void the redeemer's entitlement.
    let mut voided_buyer: Option<[u8; 32]> = buyer_id;
    if let Some(claim) = state
        .db
        .find_payment_claim_by_external_ref(author_id, provider_kind, external_ref)
        .await
        .map_err(internal)?
    {
        if state
            .db
            .void_payment_claim(&claim.code)
            .await
            .map_err(internal)?
        {
            acted = true;
        } else if let Some(redeemer) = claim.redeemed_by.as_deref()
            && let Ok(arr) = <[u8; 32]>::try_from(redeemer)
        {
            voided_buyer = Some(arr);
        }
    }

    if let Some(buyer) = voided_buyer {
        // Void the active window (sets valid_until = now → un-entitles at the
        // gates immediately; the roster row survives for the author's client
        // to prune) and drop any pending payment marker.
        if state
            .db
            .set_subscriber_valid_until(author_id, &buyer, tier_name, Some(now))
            .await
            .map_err(internal)?
        {
            acted = true;
        }
        if state
            .db
            .clear_request_payment_entitlement(author_id, &buyer, tier_name)
            .await
            .map_err(internal)?
        {
            acted = true;
        }
    }

    Ok(acted)
}

/// Why a claim redemption failed.
#[derive(Debug)]
pub enum RedeemError {
    NotFound,
    AlreadyRedeemed,
    Voided,
    Grant(GrantError),
}

/// The successful redemption outcome.
pub struct Redeemed {
    pub author_id: [u8; 32],
    pub tier: String,
    /// Epoch seconds; `None` = no expiry until refunded.
    pub valid_until: Option<i64>,
    /// `true` = enqueued for the creator's client to mint.
    pub queued: bool,
}

/// Redeem a post-payment claim code, binding the entitlement to `redeemer`
/// (monetization.md § Pillar 3 Q4 — the universal fallback binding). A repeat
/// redemption by the SAME actor is idempotent (re-applies the grant — safe
/// after a client crash mid-flow); a different actor gets `AlreadyRedeemed`.
pub async fn redeem_claim(
    state: &Arc<AppState>,
    redeemer: &[u8; 32],
    code: &str,
) -> Result<Redeemed, RedeemError> {
    let claim = state
        .db
        .get_payment_claim(code)
        .await
        .map_err(|e| RedeemError::Grant(internal(e)))?
        .ok_or(RedeemError::NotFound)?;

    if claim.voided_at.is_some() {
        return Err(RedeemError::Voided);
    }
    if let Some(prev) = claim.redeemed_by.as_deref()
        && prev != redeemer.as_slice()
    {
        return Err(RedeemError::AlreadyRedeemed);
    }

    let author_id: [u8; 32] = claim
        .author_id
        .as_slice()
        .try_into()
        .map_err(|_| RedeemError::Grant(internal("claim author_id is not 32 bytes")))?;

    // Tier existence — and subscribability: a hidden tier is `TierNotFound`
    // to the buy side (ruling 4) — is pre-checked BEFORE stamping the claim
    // redeemed, so a dangling or hidden tier mapping never consumes the claim
    // without granting.
    if !state
        .db
        .get_subscription_tier(&author_id, &claim.tier_name)
        .await
        .map_err(|e| RedeemError::Grant(internal(e)))?
        .is_some_and(|t| !t.hidden)
    {
        return Err(RedeemError::Grant(GrantError::TierNotFound));
    }

    // Stamp first-redemption atomically (the guard rejects a concurrent
    // different-actor race); a same-actor retry skips the stamp and just
    // re-applies the grant.
    if claim.redeemed_at.is_none() {
        let stamped = state
            .db
            .redeem_payment_claim(code, redeemer)
            .await
            .map_err(|e| RedeemError::Grant(internal(e)))?;
        if !stamped {
            // Lost a race: someone else redeemed/voided between read and stamp.
            return Err(RedeemError::AlreadyRedeemed);
        }
    }

    // Redemption is just another mechanism: it reduces the stored claim to the
    // same waist value webhook ingress produces — now with the buyer bound to
    // the redeemer — and hands it to the one engine entry point. The claim row
    // IS a durable, already-verified entitlement awaiting its binding.
    let entitlement = PaymentEntitlement {
        provider: claim.provider.clone(),
        payee: fauna_core::identity::ActorId(author_id),
        buyer: Buyer::Actor(fauna_core::identity::ActorId(*redeemer)),
        tier: claim.tier_name.clone(),
        valid_until_secs: claim.valid_until.map(|s| s.max(0) as u64),
        external_ref: claim.external_ref.clone(),
    };
    let queued = match apply_payment(state, &entitlement)
        .await
        .map_err(RedeemError::Grant)?
    {
        PaymentApplied::Granted { queued } => queued,
        // Unreachable: the buyer is bound to `redeemer` just above, so
        // `apply_payment` cannot take the claim-minting arm.
        other => {
            return Err(RedeemError::Grant(internal(format!(
                "claim redemption did not grant: {other:?}"
            ))));
        }
    };

    Ok(Redeemed {
        author_id,
        tier: claim.tier_name,
        valid_until: claim.valid_until,
        queued,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    /// The succession leg and the redemption door, joined by the flow rather
    /// than pinned one either side of it.
    ///
    /// `db/successions.rs` pins that the ceremony stamps `voided_at`, and the
    /// `RedeemError::Voided` arm above has always existed — but nothing asserted
    /// that the stamp is the thing the door reads, which is the whole claim of
    /// the ruling (`actor_tables.rs`, `payment_claim_codes`). A bearer code is
    /// redeemable by *any* identity, so the retired one is consulted nowhere on
    /// this path: if the ceremony's void did not land where `redeem_claim`
    /// looks, a seed thief's pre-ceremony mint would still buy access to the
    /// successor's tiers and every existing pin would stay green.
    #[tokio::test]
    async fn a_claim_minted_before_the_ceremony_no_longer_redeems_after_it() {
        let db = std::sync::Arc::new(CacheDb::open_in_memory().unwrap());
        let old: [u8; 32] = [0xA1; 32];
        let new: [u8; 32] = [0xB2; 32];
        let thief_confederate: [u8; 32] = [0xC3; 32];

        {
            let conn = db.conn().await;
            conn.execute(
                "INSERT OR IGNORE INTO tiers (name, max_inbox_bytes, max_storage_bytes, \
                 max_devices, max_blob_size) VALUES ('free', 1, 1, 1, 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO users (actor_id, tier, label, created_at, handle) \
                 VALUES (?1, 'free', '', 1, 'alice')",
                rusqlite::params![old.as_slice()],
            )
            .unwrap();
        }
        // The creator's tier, and a bearer code minted against it while the seed
        // was still good — `claims.mint` is a plain User-class gesture, so this
        // is exactly what a thief holding the seed can produce.
        db.create_subscription_tier(&old, "gold", 1, None, None, None, false, None, None, false)
            .await
            .unwrap();
        db.insert_payment_claim("code-the-thief-kept", &old, "gold", "manual", "ref", None)
            .await
            .unwrap();

        let state = std::sync::Arc::new(crate::routes::AppState::for_test(db.clone()));

        // Redeemable before the ceremony — without this the assert below could
        // pass against a code that never worked.
        assert!(
            redeem_claim(&state, &thief_confederate, "code-the-thief-kept")
                .await
                .is_ok()
        );

        // A fresh code, and the ceremony that answers the theft.
        db.insert_payment_claim("code-still-held", &old, "gold", "manual", "ref2", None)
            .await
            .unwrap();
        db.record_succession(&old, &new, b"s", 1)
            .await
            .unwrap()
            .unwrap();

        assert!(
            matches!(
                redeem_claim(&state, &thief_confederate, "code-still-held").await,
                Err(RedeemError::Voided)
            ),
            "a code minted before the ceremony must not still buy access to the \
             successor's tiers — the redemption door binds the code to whoever \
             presents it and consults the retired identity nowhere"
        );
    }

    /// Ruling 4 (`monetization.md` § The unifying model → *A tier may be
    /// hidden*) reaches the BUY side too. A hidden tier is not subscribable,
    /// and a verified payment naming one is a subscribe by another door: left
    /// ungated it would land a `payment_entitled` row, which the author's drain
    /// pump approves **without judgment** — minting the reserved owner-only
    /// tier's period key to whoever paid. The refusal is the same
    /// `TierNotFound` a nonexistent tier gets, so the buy side leaks no more
    /// than the `subscribe` door does.
    #[tokio::test]
    async fn a_verified_payment_naming_a_hidden_tier_is_refused_and_enqueues_nothing() {
        let db = std::sync::Arc::new(CacheDb::open_in_memory().unwrap());
        let author: [u8; 32] = [0xD4; 32];
        let payer: [u8; 32] = [0xE5; 32];
        let state = std::sync::Arc::new(crate::routes::AppState::for_test(db.clone()));

        // The reserved owner-only tier, exactly as the archive-import machine
        // mints it: hidden, never auto-approved.
        db.create_subscription_tier(
            &author,
            fauna_core::subscription::OWNER_ONLY_TIER,
            i64::from(fauna_core::subscription::OWNER_ONLY_TIER_RANK),
            None,
            None,
            None,
            false,
            None,
            None,
            true, // hidden
        )
        .await
        .unwrap();
        // ...and an ordinary offered tier, so the assertion below cannot pass
        // merely because this fixture's payment path is broken outright.
        db.create_subscription_tier(
            &author, "gold", 1, None, None, None, false, None, None, false,
        )
        .await
        .unwrap();

        let paid = |tier: &str| PaymentEntitlement {
            provider: "manual".into(),
            payee: fauna_core::identity::ActorId(author),
            buyer: Buyer::Actor(fauna_core::identity::ActorId(payer)),
            tier: tier.to_string(),
            valid_until_secs: None,
            external_ref: format!("ref-{tier}"),
        };

        // The offered tier enqueues, as it always has.
        assert!(
            matches!(
                apply_payment(&state, &paid("gold")).await,
                Ok(PaymentApplied::Granted { queued: true })
            ),
            "an OFFERED client-minted tier must still enqueue — without this the \
             refusal below would pass against a payment door that grants nothing"
        );

        // The hidden one is refused, and nothing lands for the author to drain.
        assert!(
            matches!(
                apply_payment(&state, &paid(fauna_core::subscription::OWNER_ONLY_TIER)).await,
                Err(GrantError::TierNotFound)
            ),
            "a payment naming a hidden tier must be indistinguishable from one \
             naming a tier that does not exist"
        );
        let queued = db.list_subscribe_requests(&author).await.unwrap();
        assert_eq!(
            queued.len(),
            1,
            "only the offered tier's request may be queued — a payment-entitled \
             row for a hidden tier is approved by the drain pump without judgment"
        );
        assert_eq!(queued[0].tier_name, "gold");
        assert!(
            queued[0].payment_entitled,
            "the offered tier's row is the drain-without-judgment kind — which is \
             precisely why the hidden tier must never reach this table"
        );

        // The unbound arm — the post-payment claim code — refuses at the mint,
        // not at the redemption: a claim naming a hidden tier could never be
        // granted, so minting it would only burn the buyer's code unredeemed.
        let unbound = |tier: &str| PaymentEntitlement {
            buyer: Buyer::Unbound,
            external_ref: format!("unbound-{tier}"),
            ..paid(tier)
        };
        assert!(
            matches!(
                apply_payment(&state, &unbound("gold")).await,
                Ok(PaymentApplied::ClaimMinted { .. })
            ),
            "an OFFERED tier still mints a claim for an unbound buyer"
        );
        assert!(
            matches!(
                apply_payment(&state, &unbound(fauna_core::subscription::OWNER_ONLY_TIER)).await,
                Err(GrantError::TierNotFound)
            ),
            "a hidden tier never mints a claim — the same answer the grant gives"
        );
    }
}
