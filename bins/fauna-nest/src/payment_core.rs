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
//! - **The purchase**: every payment records the stable per-purchase
//!   references it names (`purchase_refs` → `payment_purchases`) against its
//!   binding — the bound buyer or the minted claim — so a renewal extends,
//!   and a refund or dispute (its own provider event, with its own id and no
//!   checkout binding) voids, by the purchase.
//! - **Refund/dispute**: void the paid window (`valid_until`) wherever the
//!   entitlement currently lives (active subscriber row, pending request
//!   marker, claim). No revocation list anywhere.
//!
//! Mirrors the `invite_core` split: this module is `fauna_protocol`-free; the
//! WS-RPC handler layer (`payment_handlers`) and the HTTP webhook ingress
//! (`payment_routes`) map [`GrantError`] / [`RedeemError`] onto their own
//! error vocabularies.

use std::sync::Arc;

use fauna_payments::{Buyer, PaymentEntitlement};

use crate::db::now_epoch_secs;
use crate::routes::AppState;
use crate::subscription_handlers::SubscriberHome;

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
    apply_payment_from(state, entitlement, None).await
}

/// [`apply_payment`] for a buyer who may live on another nest: `home` is
/// `Some` exactly when a claim is redeemed across nests (`federated-follow.md`
/// § Decision 2 → *The redeem twin*) — the payment-entitled request row then
/// carries the buyer's home, as a relayed subscribe's does, so the approval
/// copies it onto the roster and the state leg, delivery and the key-blob
/// relay find it.
pub(crate) async fn apply_payment_from(
    state: &Arc<AppState>,
    entitlement: &PaymentEntitlement,
    home: Option<SubscriberHome<'_>>,
) -> Result<PaymentApplied, GrantError> {
    let payee = &entitlement.payee.0;
    let valid_until = valid_until_of(entitlement);

    // A purchase seen before (a renewal, a redelivery under a new event id,
    // or — only while the mapping names no product — the checkout completing
    // after its first invoice) resolves to the binding it already has: the
    // bound buyer, the redeemer of its claim, or its still-open claim, which
    // the renewal re-stamps. With a product set, a first invoice arriving
    // before its checkout names no product and no recorded purchase, so the
    // ingress acknowledges and drops it and the checkout mints.
    let mut buyer = entitlement.buyer.clone();
    if matches!(buyer, Buyer::Unbound)
        && let Some(found) = resolve_purchase(state, entitlement, valid_until).await?
    {
        match found {
            PurchaseBinding::Buyer(b) => buyer = Buyer::Actor(fauna_core::identity::ActorId(b)),
            PurchaseBinding::OpenClaim { code } => {
                return Ok(PaymentApplied::ClaimExists { code });
            }
        }
    }

    match &buyer {
        Buyer::Actor(b) => {
            let queued =
                grant_paid_entitlement(state, payee, &b.0, &entitlement.tier, valid_until, home)
                    .await?;
            record_purchase(state, entitlement, Some(&b.0), None).await?;
            Ok(PaymentApplied::Granted { queued })
        }
        Buyer::Unbound => {
            let applied = mint_claim(state, entitlement, valid_until).await?;
            record_purchase(state, entitlement, None, Some(&entitlement.external_ref)).await?;
            Ok(applied)
        }
    }
}

/// What a purchase reference an earlier payment recorded is bound to.
enum PurchaseBinding {
    /// The bound buyer, or the actor who redeemed the purchase's claim.
    Buyer([u8; 32]),
    /// The purchase's claim is still unredeemed (and has just been re-stamped
    /// with this payment's window).
    OpenClaim { code: String },
}

/// Resolve an unbound payment through the purchase it names
/// (`payment_purchases`), if an earlier payment recorded one.
async fn resolve_purchase(
    state: &Arc<AppState>,
    entitlement: &PaymentEntitlement,
    valid_until: Option<i64>,
) -> Result<Option<PurchaseBinding>, GrantError> {
    let payee = &entitlement.payee.0;
    let Some(purchase) = state
        .db
        .find_payment_purchase(payee, &entitlement.provider, &entitlement.purchase_refs)
        .await
        .map_err(internal)?
    else {
        return Ok(None);
    };
    if let Some(b) = purchase
        .buyer_actor_id
        .as_deref()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
    {
        return Ok(Some(PurchaseBinding::Buyer(b)));
    }
    let Some(ext) = purchase.claim_external_ref.as_deref() else {
        return Ok(None);
    };
    let Some(claim) = state
        .db
        .find_payment_claim_by_external_ref(payee, &entitlement.provider, ext)
        .await
        .map_err(internal)?
    else {
        return Ok(None);
    };
    if claim.voided_at.is_some() {
        // A voided claim binds nothing: this payment is a fresh purchase.
        return Ok(None);
    }
    if let Some(r) = claim
        .redeemed_by
        .as_deref()
        .and_then(|r| <[u8; 32]>::try_from(r).ok())
    {
        return Ok(Some(PurchaseBinding::Buyer(r)));
    }
    if state
        .db
        .renew_payment_claim(&claim.code, valid_until, entitlement.return_ref.as_deref())
        .await
        .map_err(internal)?
    {
        record_purchase(state, entitlement, None, Some(ext)).await?;
        return Ok(Some(PurchaseBinding::OpenClaim { code: claim.code }));
    }
    // Redeemed or voided between the read and the stamp: read it again on
    // the next delivery; this one mints afresh rather than guess.
    Ok(None)
}

/// Record every purchase reference this payment named against its binding.
async fn record_purchase(
    state: &Arc<AppState>,
    entitlement: &PaymentEntitlement,
    buyer: Option<&[u8; 32]>,
    claim_external_ref: Option<&str>,
) -> Result<(), GrantError> {
    if entitlement.purchase_refs.is_empty() {
        return Ok(());
    }
    state
        .db
        .record_payment_purchase(
            &entitlement.payee.0,
            &entitlement.provider,
            &entitlement.purchase_refs,
            &entitlement.tier,
            entitlement.product_reference.as_deref(),
            buyer,
            claim_external_ref,
        )
        .await
        .map_err(internal)
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
            .insert_payment_claim_with_return_ref(
                &code,
                payee,
                &entitlement.tier,
                &entitlement.provider,
                &entitlement.external_ref,
                valid_until,
                entitlement.return_ref.as_deref(),
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
/// `false` = the subscription is active now (a window re-stamped on a roster
/// row that still stands with no removal queued); `true` = enqueued for the
/// creator's client to mint.
///
/// Private on purpose: mechanisms reach the engine through
/// [`apply_payment`]'s waist value, never by assembling loose scalars.
async fn grant_paid_entitlement(
    state: &Arc<AppState>,
    author_id: &[u8; 32],
    buyer_id: &[u8; 32],
    tier_name: &str,
    valid_until: Option<i64>,
    home: Option<SubscriberHome<'_>>,
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

    // A renewal: the buyer's roster row for the tier still stands — its window
    // passed or not — and no removal of it is queued, so the key is still
    // wrapped to them and re-stamping the window in place is the whole grant.
    // No re-approve, no queue round-trip; an hour-late recurring renewal is a
    // renewal, not a request (`monetization.md` § Pillar 3 → *Expiry &
    // revocation*: inside the lapse grace the standing row is re-stamped). Once
    // a removal is queued (a void, or a lapse past the grace) or the row is
    // gone, the payment lands as a first purchase does — the request path below.
    if !state
        .db
        .has_pending_unsubscribe_request(author_id, buyer_id, tier_name)
        .await
        .map_err(internal)?
        && state
            .db
            .set_subscriber_valid_until(author_id, buyer_id, tier_name, valid_until)
            .await
            .map_err(internal)?
    {
        return Ok(false);
    }

    // The nest holds no key material: enqueue exactly like a
    // subscriber-initiated request, marked payment-entitled so the author's
    // drain pump approves it without creator judgment.
    state
        .db
        .upsert_payment_entitled_request(author_id, buyer_id, tier_name, valid_until, home)
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
        .enqueue_unlock_fanout(author_id, buyer_id, tier_name, home)
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

/// What applying a refund did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RefundApplied {
    /// Whether anything was voided (`false` = the event was authentic but
    /// matched nothing — e.g. a refund for an unknown payment; acknowledged,
    /// no action).
    pub acted: bool,
    /// Every buyer whose paid window was voided — the actors a membership
    /// lapse reconcile must look at (monetization.md § Pillar 4 Rail C).
    pub voided_buyers: Vec<[u8; 32]>,
}

/// Apply a refund/dispute: void the paid window wherever the entitlement
/// currently lives.
///
/// Takes the same waist value [`apply_payment`] does — a refund names exactly
/// the entitlement it voids, so the mechanism produces one shape for both
/// verbs. It finds that entitlement two ways: by the **purchase** the refund
/// names (`purchase_refs` → `payment_purchases`: a provider's refund or
/// dispute is its own event, carrying no checkout binding and a delivery id
/// of its own), and by the claim minted for the very event id it carries (a
/// mechanism whose refund names the payment's own id, as the fake provider's
/// does).
pub async fn apply_refund(
    state: &Arc<AppState>,
    entitlement: &PaymentEntitlement,
) -> Result<RefundApplied, GrantError> {
    let author_id = &entitlement.payee.0;
    let provider_kind = entitlement.provider.as_str();
    let mut out = RefundApplied::default();

    // The purchase arm: the binding an earlier payment recorded.
    if let Some(purchase) = state
        .db
        .find_payment_purchase(author_id, provider_kind, &entitlement.purchase_refs)
        .await
        .map_err(internal)?
    {
        let mut buyer = purchase
            .buyer_actor_id
            .as_deref()
            .and_then(|b| <[u8; 32]>::try_from(b).ok());
        if let Some(ext) = purchase.claim_external_ref.as_deref()
            && let Some(claim) = state
                .db
                .find_payment_claim_by_external_ref(author_id, provider_kind, ext)
                .await
                .map_err(internal)?
        {
            let (claim_acted, redeemer) = void_claim_row(state, &claim).await?;
            out.acted |= claim_acted;
            buyer = buyer.or(redeemer);
        }
        if let Some(b) = buyer {
            out.acted |= void_entitlement_window(state, author_id, &b, &purchase.tier_name).await?;
            out.voided_buyers.push(b);
        }
        out.acted |= state
            .db
            .void_payment_purchase_refs(author_id, provider_kind, &entitlement.purchase_refs)
            .await
            .map_err(internal)?
            > 0;
    }

    // The event-id arm: a claim minted for this payment: void it if
    // unredeemed; if already redeemed, fall through to void the redeemer's
    // entitlement.
    let mut voided_buyer: Option<[u8; 32]> = match &entitlement.buyer {
        Buyer::Actor(a) => Some(a.0),
        Buyer::Unbound => None,
    };
    if let Some(claim) = state
        .db
        .find_payment_claim_by_external_ref(author_id, provider_kind, &entitlement.external_ref)
        .await
        .map_err(internal)?
    {
        let (claim_acted, redeemer) = void_claim_row(state, &claim).await?;
        out.acted |= claim_acted;
        if redeemer.is_some() {
            voided_buyer = redeemer;
        }
    }
    if let Some(buyer) = voided_buyer {
        out.acted |= void_entitlement_window(state, author_id, &buyer, &entitlement.tier).await?;
        if !out.voided_buyers.contains(&buyer) {
            out.voided_buyers.push(buyer);
        }
    }

    Ok(out)
}

/// The claim arm a refund and an author's void ([`void_claim`]) share: stamp
/// `voided_at` on the claim. An unredeemed claim is voided outright; a
/// redeemed one is voided too — the bound code is also the visitor's web
/// credential, which honours only `voided_at` (monetization.md § Pillar 2 →
/// *Visitor entitlement doors* ruling (1)) — and its redeemer is returned so
/// the caller voids their entitlement window as well. Returns whether a stamp
/// landed, and the redeemer when the claim was a redeemed one.
async fn void_claim_row(
    state: &Arc<AppState>,
    claim: &crate::db::PaymentClaimRow,
) -> Result<(bool, Option<[u8; 32]>), GrantError> {
    if state
        .db
        .void_payment_claim(&claim.code)
        .await
        .map_err(internal)?
    {
        return Ok((true, None));
    }
    if let Some(redeemer) = claim.redeemed_by.as_deref()
        && let Ok(arr) = <[u8; 32]>::try_from(redeemer)
    {
        let stamped = state
            .db
            .void_redeemed_payment_claim(&claim.code)
            .await
            .map_err(internal)?;
        return Ok((stamped, Some(arr)));
    }
    Ok((false, None))
}

/// Void `buyer`'s paid window on `author_id`'s tier: `valid_until = now`
/// un-entitles at the gates immediately, any pending payment marker is
/// dropped (so the author's client never approves a voided payment later),
/// and — the money having gone back — the removal a leave takes is queued
/// for the roster row, its home carried on the request, so the author's drain
/// rotates the buyer out and a remote buyer's nest reads `removed`
/// (`monetization.md` § Pillar 3 → *Expiry & revocation*, the 2026-11-09
/// ruling). A membership tier queues no removal: its window's end is Rail C's
/// downgrade (§ Pillar 4). A refund or dispute ([`apply_refund`]) and the
/// author's void of a redeemed code ([`void_claim`]) both arrive here.
/// Returns whether anything changed.
async fn void_entitlement_window(
    state: &Arc<AppState>,
    author_id: &[u8; 32],
    buyer: &[u8; 32],
    tier_name: &str,
) -> Result<bool, GrantError> {
    let windowed = state
        .db
        .set_subscriber_valid_until(author_id, buyer, tier_name, Some(now_epoch_secs()))
        .await
        .map_err(internal)?;
    let unmarked = state
        .db
        .clear_request_payment_entitlement(author_id, buyer, tier_name)
        .await
        .map_err(internal)?;
    if windowed
        && state
            .db
            .get_membership_tier(author_id, tier_name)
            .await
            .map_err(internal)?
            .is_none()
    {
        let home = state
            .db
            .subscriber_home_with_url(author_id, buyer, tier_name)
            .await
            .map_err(internal)?;
        let home = home.as_ref().map(|(nest_id, url)| SubscriberHome {
            nest_id,
            url: url.as_deref(),
        });
        crate::subscription_handlers::enqueue_removal(state, author_id, buyer, tier_name, home)
            .await
            .map_err(internal)?;
    }
    Ok(windowed || unmarked)
}

/// Why an author's claim void failed.
#[derive(Debug)]
pub enum VoidError {
    /// No such code, or a code `author` did not mint — the two answer alike,
    /// so another author's codes are never acknowledged.
    NotFound,
    Grant(GrantError),
}

/// The author withdraws one of their own claim codes
/// (`fauna.payments.claims.void`; monetization.md § Pillar 3 → *Buyer↔actor
/// binding (Q4)*). The refund path's claim arm without the provider event: an
/// unredeemed code is voided (refused thereafter at `claims.redeem` and the
/// web redeem door); a redeemed one is voided and its redeemer's window on
/// the claim's tier is voided as [`apply_refund`] voids it — like a refund,
/// that window is the redeemer's entitlement to the tier whatever granted it.
///
/// Idempotent: a code already voided (by an earlier void or by a refund)
/// changes nothing and answers its existing stamp, so a repeat can never
/// reach an entitlement the redeemer gained afresh after the first void.
/// Returns the claim's `voided_at` (epoch seconds).
pub async fn void_claim(
    state: &Arc<AppState>,
    author_id: &[u8; 32],
    code: &str,
) -> Result<i64, VoidError> {
    let read = || async {
        state
            .db
            .get_payment_claim(code)
            .await
            .map_err(|e| VoidError::Grant(internal(e)))?
            .filter(|c| c.author_id.as_slice() == author_id.as_slice())
            .ok_or(VoidError::NotFound)
    };
    let mut claim = read().await?;
    // Two passes: a redemption landing between the read and the void stamp
    // makes the first pass's unredeemed-arm stamp miss, and the re-read then
    // carries the redeemer the redeemed arm needs.
    for _ in 0..2 {
        if let Some(at) = claim.voided_at {
            return Ok(at);
        }
        let (_, redeemer) = void_claim_row(state, &claim)
            .await
            .map_err(VoidError::Grant)?;
        if let Some(buyer) = redeemer {
            void_entitlement_window(state, author_id, &buyer, &claim.tier_name)
                .await
                .map_err(VoidError::Grant)?;
        }
        claim = read().await?;
    }
    claim
        .voided_at
        .ok_or_else(|| VoidError::Grant(internal("claim void did not land")))
}

/// Why a claim redemption failed.
#[derive(Debug)]
pub enum RedeemError {
    NotFound,
    AlreadyRedeemed,
    Voided,
    /// A membership tier's code redeemed from another nest
    /// (`fauna.payments.claim_admits_elsewhere`): the claim is left unstamped.
    AdmitsElsewhere,
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
    redeem_claim_from(state, redeemer, code, None).await
}

/// [`redeem_claim`] for a redeemer who may live on another nest — the redeem
/// twin's body (`federated-follow.md` § Decision 2 → *The redeem twin*): with
/// `home` set, the payment-entitled request carries the redeemer's home, and a
/// code whose tier is designated for nest membership (§ Pillar 4) is refused
/// [`RedeemError::AdmitsElsewhere`] BEFORE the claim is stamped — admission is
/// registration on this nest, which a redeem across nests never grants.
pub(crate) async fn redeem_claim_from(
    state: &Arc<AppState>,
    redeemer: &[u8; 32],
    code: &str,
    home: Option<SubscriberHome<'_>>,
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
    if home.is_some()
        && state
            .db
            .get_membership_tier(&author_id, &claim.tier_name)
            .await
            .map_err(|e| RedeemError::Grant(internal(e)))?
            .is_some()
    {
        return Err(RedeemError::AdmitsElsewhere);
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
        return_ref: None,
        purchase_refs: Vec::new(),
        product_reference: None,
    };
    let queued = match apply_payment_from(state, &entitlement, home)
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

// ── Claim → resource entitlement (the web paywall's redeem door) ──

/// The tier facts the claim-entitlement decision reads.
#[derive(Debug, Clone, Copy)]
pub struct TierFacts<'a> {
    pub name: &'a str,
    pub rank: i64,
    /// `unlocks_post` is set (a per-post pay-to-unlock tier).
    pub designated: bool,
    pub hidden: bool,
}

/// Does a claim for tier `claim` entitle a resource gated to tier `resource`?
///
/// The cascade predicate the grant-time fan-out applies to actors
/// ([`crate::db::CacheDb::enqueue_unlock_fanout`]), evaluated against the
/// claim's tier instead of a `subscribers` row (`monetization.md` § Pillar 2
/// → *Visitor entitlement doors* ruling (4), § Per-post pay-to-unlock rules
/// (a)–(e)): the same tier always; otherwise a designated claim reaches
/// nothing else (rule (a), re-ruled 2026-11-07 — a purchase is a good, not a
/// relationship), the target must sit at or below the claim's rank, and a
/// hidden target is never reached (rule (e)). Rules (b)–(d) govern the fan-out's
/// delivery (perpetual enrolment, the creation-time door, readability
/// suppression) and have no stateless counterpart: the claim's own window is
/// the door's `valid_until` check.
pub fn claim_tier_entitles(claim: TierFacts<'_>, resource: TierFacts<'_>) -> bool {
    if claim.name == resource.name {
        return true;
    }
    !(claim.designated || resource.hidden || resource.rank > claim.rank)
}

/// [`claim_tier_entitles`] over `owner`'s live tier rows. A tier that no
/// longer exists entitles nothing and is entitled by nothing (fail closed).
pub async fn claim_entitles(
    db: &crate::db::CacheDb,
    owner: &[u8; 32],
    claim_tier: &str,
    resource_tier: &str,
) -> anyhow::Result<bool> {
    let (Some(claim), Some(resource)) = (
        db.get_subscription_tier(owner, claim_tier).await?,
        db.get_subscription_tier(owner, resource_tier).await?,
    ) else {
        return Ok(false);
    };
    Ok(claim_tier_entitles(
        tier_facts(&claim),
        tier_facts(&resource),
    ))
}

fn tier_facts(t: &crate::db::SubscriptionTierRow) -> TierFacts<'_> {
    TierFacts {
        name: &t.name,
        rank: t.rank,
        designated: t.unlocks_post.is_some(),
        hidden: t.hidden,
    }
}

#[cfg(test)]
mod claim_entitlement_tests {
    use super::*;

    fn tier(name: &str, rank: i64) -> TierFacts<'_> {
        TierFacts {
            name,
            rank,
            designated: false,
            hidden: false,
        }
    }

    #[test]
    fn the_same_tier_always_entitles() {
        let sold = TierFacts {
            designated: true,
            ..tier("post-1", 5)
        };
        assert!(claim_tier_entitles(sold, sold));
    }

    #[test]
    fn an_undesignated_claim_reaches_every_tier_at_or_below_its_rank() {
        assert!(claim_tier_entitles(tier("gold", 3), tier("silver", 2)));
        assert!(claim_tier_entitles(tier("gold", 3), tier("peer", 3)));
        assert!(!claim_tier_entitles(tier("silver", 2), tier("gold", 3)));
        // A sold post at a rank the subscription covers is included free.
        let sold = TierFacts {
            designated: true,
            ..tier("post-1", 1)
        };
        assert!(claim_tier_entitles(tier("gold", 3), sold));
    }

    #[test]
    fn rule_a_every_designated_edge_is_blocked() {
        let cheap = TierFacts {
            designated: true,
            ..tier("post-cheap", 5)
        };
        let other = TierFacts {
            designated: true,
            ..tier("post-other", 1)
        };
        assert!(!claim_tier_entitles(cheap, other));
    }

    #[test]
    fn a_designated_claim_entitles_only_its_own_tier() {
        // Rule (a), re-ruled 2026-11-07: a purchase enrolls the buyer in
        // nothing else — not followers, and not a paid tier below a pure
        // pay-per-view sale minted above it.
        let pay_per_view = TierFacts {
            designated: true,
            ..tier("post-unlock-x", 3)
        };
        assert!(!claim_tier_entitles(pay_per_view, tier("followers", 0)));
        assert!(!claim_tier_entitles(pay_per_view, tier("gold", 2)));
        assert!(claim_tier_entitles(pay_per_view, pay_per_view));
    }

    #[test]
    fn rule_e_a_hidden_tier_is_never_reached() {
        let hidden = TierFacts {
            hidden: true,
            ..tier("only-me", 0)
        };
        assert!(!claim_tier_entitles(tier("gold", 3), hidden));
    }
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
    async fn a_refund_voids_an_already_redeemed_claim_so_its_code_stops_opening_the_web() {
        // The claim code is also the visitor's web credential, which honours
        // only `voided_at` (monetization.md § Pillar 2 → *Visitor entitlement
        // doors* ruling (1)) — so a refund after binding must stamp it too,
        // not only void the redeemer's subscriber window.
        let db = std::sync::Arc::new(CacheDb::open_in_memory().unwrap());
        let author: [u8; 32] = [0xA1; 32];
        let buyer: [u8; 32] = [0xB2; 32];
        db.insert_payment_claim("CODE-BOUND", &author, "gold", "fake", "pay_r", None)
            .await
            .unwrap();
        assert!(db.redeem_payment_claim("CODE-BOUND", &buyer).await.unwrap());
        let state = std::sync::Arc::new(crate::routes::AppState::for_test(db.clone()));
        let refund = PaymentEntitlement {
            provider: "fake".into(),
            payee: fauna_core::identity::ActorId(author),
            buyer: Buyer::Unbound,
            tier: "gold".into(),
            valid_until_secs: None,
            external_ref: "pay_r".into(),
            return_ref: None,
            purchase_refs: Vec::new(),
            product_reference: None,
        };
        assert!(apply_refund(&state, &refund).await.unwrap().acted);
        let claim = db.get_payment_claim("CODE-BOUND").await.unwrap().unwrap();
        assert!(claim.voided_at.is_some(), "a refunded bound code is voided");
        assert!(claim.redeemed_at.is_some(), "the binding stays on record");
    }

    #[tokio::test]
    async fn a_webhook_minted_claim_is_found_by_its_return_ref_on_its_own_site_only() {
        let db = CacheDb::open_in_memory().unwrap();
        let author: [u8; 32] = [0xA1; 32];
        let other: [u8; 32] = [0xC3; 32];
        db.insert_payment_claim_with_return_ref(
            "CODE-RET",
            &author,
            "gold",
            "stripe",
            "evt_1",
            None,
            Some("cs_1"),
        )
        .await
        .unwrap();
        let hit = db
            .find_payment_claim_by_return_ref(&author, "stripe", "cs_1")
            .await
            .unwrap();
        assert_eq!(hit.map(|c| c.code).as_deref(), Some("CODE-RET"));
        for (who, provider, r) in [
            (&other, "stripe", "cs_1"),
            (&author, "fake", "cs_1"),
            (&author, "stripe", "cs_2"),
        ] {
            assert!(
                db.find_payment_claim_by_return_ref(who, provider, r)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    }

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
            return_ref: None,
            purchase_refs: Vec::new(),
            product_reference: None,
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
            return_ref: None,
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

    // ── A Stripe purchase, end to end ──────
    //
    // Each event below goes through the real adapter (`StripeProvider::verify`
    // over a signed body) and the waist, exactly as the webhook ingress feeds
    // the engine — because the defect lived BETWEEN them: the adapter set every
    // event's `external_ref` to its own `evt_` id, and the refund looked the
    // claim up by that id, so a refund never found its checkout.

    const STRIPE_SECRET: &str = "whsec_row_3723";

    fn stripe_event(
        author: [u8; 32],
        evt: &str,
        ty: &str,
        object: serde_json::Value,
    ) -> (fauna_payments::PaymentEventKind, PaymentEntitlement) {
        use fauna_payments::PaymentProvider;
        let body = serde_json::to_vec(&serde_json::json!({
            "id": evt, "type": ty, "data": { "object": object },
        }))
        .unwrap();
        let now = now_epoch_secs() as u64;
        let headers = fauna_payments::WebhookHeaders::from_pairs([(
            fauna_payments::stripe::SIGNATURE_HEADER,
            fauna_payments::stripe::signature_header(STRIPE_SECRET, now, &body),
        )]);
        let ev = fauna_payments::stripe::StripeProvider
            .verify(STRIPE_SECRET, &headers, &body, now)
            .expect("a signed Stripe body verifies");
        let mapping = fauna_payments::TierMapping {
            tier: "gold".into(),
            product: None,
        };
        let e = PaymentEntitlement::from_verified_event(
            "stripe",
            fauna_core::identity::ActorId(author),
            &ev,
            &mapping,
        )
        .expect("the single mapping covers it");
        (ev.kind, e)
    }

    async fn stripe_state(author: [u8; 32]) -> (Arc<CacheDb>, Arc<AppState>) {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        db.create_subscription_tier(
            &author, "gold", 1, None, None, None, false, None, None, false,
        )
        .await
        .unwrap();
        let state = Arc::new(AppState::for_test(db.clone()));
        (db, state)
    }

    fn minted_code(applied: PaymentApplied) -> String {
        match applied {
            PaymentApplied::ClaimMinted { code } => code,
            other => panic!("expected a minted claim, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_stripe_refund_after_an_unbound_checkout_voids_that_checkouts_claim() {
        let author = [0xA1; 32];
        let (db, state) = stripe_state(author).await;
        let (kind, paid) = stripe_event(
            author,
            "evt_checkout",
            "checkout.session.completed",
            serde_json::json!({ "id": "cs_1", "payment_status": "paid", "payment_intent": "pi_1" }),
        );
        assert_eq!(kind, fauna_payments::PaymentEventKind::Payment);
        let code = minted_code(apply_payment(&state, &paid).await.unwrap());
        // The visitor redeems it on the web (the bound code opens the paywall).
        let buyer = [0xB2; 32];
        assert!(db.redeem_payment_claim(&code, &buyer).await.unwrap());

        // The refund is its own event: its own `evt_`, a charge object, no
        // `client_reference_id` — it names the purchase only by its intent.
        let (kind, refund) = stripe_event(
            author,
            "evt_refund",
            "charge.refunded",
            serde_json::json!({ "id": "ch_1", "payment_intent": "pi_1" }),
        );
        assert_eq!(kind, fauna_payments::PaymentEventKind::Refund);
        let applied = apply_refund(&state, &refund).await.unwrap();
        assert!(applied.acted, "the refund found the checkout's purchase");
        assert_eq!(applied.voided_buyers, vec![buyer]);
        let claim = db.get_payment_claim(&code).await.unwrap().unwrap();
        assert!(claim.voided_at.is_some(), "the claim stops opening the web");

        // A dispute on an UNREDEEMED claim voids it outright.
        let (_, paid2) = stripe_event(
            author,
            "evt_checkout2",
            "checkout.session.completed",
            serde_json::json!({ "id": "cs_2", "payment_status": "paid", "payment_intent": "pi_2" }),
        );
        let code2 = minted_code(apply_payment(&state, &paid2).await.unwrap());
        let (_, dispute) = stripe_event(
            author,
            "evt_dispute",
            "charge.dispute.created",
            serde_json::json!({ "id": "dp_1", "charge": "ch_2", "payment_intent": "pi_2" }),
        );
        assert!(apply_refund(&state, &dispute).await.unwrap().acted);
        let claim2 = db.get_payment_claim(&code2).await.unwrap().unwrap();
        assert!(claim2.voided_at.is_some() && claim2.redeemed_at.is_none());
    }

    #[tokio::test]
    async fn a_stripe_refund_after_a_bound_checkout_voids_the_buyers_entitlement() {
        let author = [0xA1; 32];
        let buyer = [0xB3; 32];
        let (_db, state) = stripe_state(author).await;
        let (_, paid) = stripe_event(
            author,
            "evt_checkout",
            "checkout.session.completed",
            serde_json::json!({ "id": "cs_1", "payment_status": "paid", "payment_intent": "pi_1",
                                "client_reference_id": hex::encode(buyer) }),
        );
        assert_eq!(
            apply_payment(&state, &paid).await.unwrap(),
            PaymentApplied::Granted { queued: true }
        );
        let (_, refund) = stripe_event(
            author,
            "evt_refund",
            "charge.refunded",
            serde_json::json!({ "id": "ch_1", "payment_intent": "pi_1" }),
        );
        let applied = apply_refund(&state, &refund).await.unwrap();
        assert!(applied.acted, "the pending payment marker is dropped");
        assert_eq!(applied.voided_buyers, vec![buyer]);
    }

    #[tokio::test]
    async fn a_delayed_payment_mints_on_success_and_its_failure_voids_the_claim() {
        let author = [0xA1; 32];
        let (db, state) = stripe_state(author).await;
        let object = serde_json::json!({ "id": "cs_1", "payment_status": "unpaid",
                                         "payment_intent": "pi_1" });
        // The unpaid completion is no payment: the engine is never reached.
        let (kind, _) = stripe_event(
            author,
            "evt_c",
            "checkout.session.completed",
            object.clone(),
        );
        assert_eq!(kind, fauna_payments::PaymentEventKind::Ignored);
        assert!(db.list_payment_claims(&author).await.unwrap().is_empty());

        let (kind, settled) = stripe_event(
            author,
            "evt_ok",
            "checkout.session.async_payment_succeeded",
            object.clone(),
        );
        assert_eq!(kind, fauna_payments::PaymentEventKind::Payment);
        let code = minted_code(apply_payment(&state, &settled).await.unwrap());
        // The return door finds it by the session id.
        assert_eq!(
            db.find_payment_claim_by_return_ref(&author, "stripe", "cs_1")
                .await
                .unwrap()
                .map(|c| c.code),
            Some(code.clone())
        );

        let (kind, failed) = stripe_event(
            author,
            "evt_fail",
            "checkout.session.async_payment_failed",
            object,
        );
        assert_eq!(kind, fauna_payments::PaymentEventKind::Refund);
        assert!(apply_refund(&state, &failed).await.unwrap().acted);
        let claim = db.get_payment_claim(&code).await.unwrap().unwrap();
        assert!(claim.voided_at.is_some());
    }

    #[tokio::test]
    async fn a_renewal_extends_the_purchases_open_claim_instead_of_minting_another() {
        let author = [0xA1; 32];
        let (db, state) = stripe_state(author).await;
        let (_, paid) = stripe_event(
            author,
            "evt_checkout",
            "checkout.session.completed",
            serde_json::json!({ "id": "cs_1", "payment_status": "paid", "subscription": "sub_1" }),
        );
        let code = minted_code(apply_payment(&state, &paid).await.unwrap());
        let (_, renewal) = stripe_event(
            author,
            "evt_invoice",
            "invoice.paid",
            serde_json::json!({ "id": "in_2", "subscription": "sub_1", "payment_intent": "pi_2",
                                "current_period_end": 1_900_000_000u64 }),
        );
        assert_eq!(
            apply_payment(&state, &renewal).await.unwrap(),
            PaymentApplied::ClaimExists { code: code.clone() }
        );
        assert_eq!(db.list_payment_claims(&author).await.unwrap().len(), 1);
        let claim = db.get_payment_claim(&code).await.unwrap().unwrap();
        assert_eq!(claim.valid_until, Some(1_900_000_000));

        // A refund of the renewal's charge names its intent, which the
        // renewal recorded against the same purchase.
        let (_, refund) = stripe_event(
            author,
            "evt_refund",
            "charge.refunded",
            serde_json::json!({ "id": "ch_2", "payment_intent": "pi_2" }),
        );
        assert!(apply_refund(&state, &refund).await.unwrap().acted);
        assert!(
            db.get_payment_claim(&code)
                .await
                .unwrap()
                .unwrap()
                .voided_at
                .is_some()
        );
    }

    // ── The end of a paid window queues the leave's removal ────────────
    //
    // `monetization.md` § Pillar 3 → *Expiry & revocation*, the 2026-11-09
    // ruling: a void queues the `kind='unsubscribe'` row at once, a lapse
    // queues it past the grace, a renewal inside the grace re-stamps in place.

    const AUTHOR: [u8; 32] = [0x7A; 32];
    const BUYER: [u8; 32] = [0x7B; 32];

    /// An author with a `gold` content tier and `BUYER` on its roster,
    /// `valid_until` as given.
    async fn roster_fixture(valid_until: Option<i64>) -> (Arc<CacheDb>, Arc<AppState>) {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        db.create_subscription_tier(
            &AUTHOR, "gold", 1, None, None, None, false, None, None, false,
        )
        .await
        .unwrap();
        db.add_subscriber(&AUTHOR, &BUYER, "gold", None)
            .await
            .unwrap();
        db.set_subscriber_valid_until(&AUTHOR, &BUYER, "gold", valid_until)
            .await
            .unwrap();
        let state = Arc::new(AppState::for_test(db.clone()));
        (db, state)
    }

    fn bound(tier: &str, valid_until: Option<u64>, external_ref: &str) -> PaymentEntitlement {
        PaymentEntitlement {
            provider: "fake".into(),
            payee: fauna_core::identity::ActorId(AUTHOR),
            buyer: Buyer::Actor(fauna_core::identity::ActorId(BUYER)),
            tier: tier.into(),
            valid_until_secs: valid_until,
            external_ref: external_ref.into(),
            return_ref: None,
            purchase_refs: Vec::new(),
            product_reference: None,
        }
    }

    /// `(kind, subscriber, tier)` of every request row queued for `AUTHOR`.
    async fn queued(db: &CacheDb) -> Vec<(String, Vec<u8>, String)> {
        db.list_subscribe_requests(&AUTHOR)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.kind, r.subscriber_id, r.tier_name))
            .collect()
    }

    fn removal_of_buyer() -> (String, Vec<u8>, String) {
        ("unsubscribe".into(), BUYER.to_vec(), "gold".into())
    }

    /// The `free` quota tier a membership designation's foreign keys name.
    async fn seed_free_quota_tier(db: &CacheDb) {
        db.conn()
            .await
            .execute(
                "INSERT OR IGNORE INTO tiers (name, max_inbox_bytes, max_storage_bytes, \
                 max_devices, max_blob_size) VALUES ('free', 1, 1, 1, 1)",
                [],
            )
            .unwrap();
    }

    #[tokio::test]
    async fn a_refund_queues_the_leaves_removal_carrying_the_roster_rows_home() {
        let (db, state) = roster_fixture(Some(now_epoch_secs() + 86_400)).await;
        let home: [u8; 32] = [0x99; 32];
        db.conn()
            .await
            .execute(
                "UPDATE subscribers SET home_nest_id = ?1, home_nest_url = 'https://f.example'
                  WHERE author_id = ?2 AND subscriber_id = ?3",
                rusqlite::params![home.as_slice(), AUTHOR.as_slice(), BUYER.as_slice()],
            )
            .unwrap();

        assert!(
            apply_refund(&state, &bound("gold", None, "pay_1"))
                .await
                .unwrap()
                .acted
        );

        assert!(!db.is_subscriber(&AUTHOR, &BUYER, "gold").await.unwrap());
        let rows = db.list_subscribe_requests(&AUTHOR).await.unwrap();
        assert_eq!(rows.len(), 1, "exactly the removal: {rows:?}");
        assert_eq!(
            (
                rows[0].kind.as_str(),
                rows[0].subscriber_id.as_slice(),
                rows[0].tier_name.as_str()
            ),
            ("unsubscribe", BUYER.as_slice(), "gold")
        );
        assert_eq!(rows[0].home_nest_url.as_deref(), Some("https://f.example"));
        assert_eq!(
            db.subscribe_request_home(rows[0].id).await.unwrap(),
            Some(home)
        );

        // A repeat delivery of the refund queues no second row.
        apply_refund(&state, &bound("gold", None, "pay_1"))
            .await
            .unwrap();
        assert_eq!(queued(&db).await, vec![removal_of_buyer()]);
    }

    #[tokio::test]
    async fn the_authors_void_of_a_redeemed_code_queues_the_removal() {
        let (db, state) = roster_fixture(Some(now_epoch_secs() + 86_400)).await;
        db.insert_payment_claim("CODE-V", &AUTHOR, "gold", "manual", "ref_v", None)
            .await
            .unwrap();
        assert!(db.redeem_payment_claim("CODE-V", &BUYER).await.unwrap());

        void_claim(&state, &AUTHOR, "CODE-V").await.unwrap();

        assert!(!db.is_subscriber(&AUTHOR, &BUYER, "gold").await.unwrap());
        assert_eq!(queued(&db).await, vec![removal_of_buyer()]);
    }

    #[tokio::test]
    async fn a_membership_tiers_refund_queues_no_removal() {
        let (db, state) = roster_fixture(Some(now_epoch_secs() + 86_400)).await;
        seed_free_quota_tier(&db).await;
        db.upsert_membership_tier(&AUTHOR, "gold", "free", "free")
            .await
            .unwrap();

        assert!(
            apply_refund(&state, &bound("gold", None, "pay_1"))
                .await
                .unwrap()
                .acted
        );

        assert!(
            queued(&db).await.is_empty(),
            "a membership's end is Rail C's downgrade, never a removal"
        );
    }

    #[tokio::test]
    async fn a_refund_for_a_buyer_with_no_roster_row_queues_none() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        db.create_subscription_tier(
            &AUTHOR, "gold", 1, None, None, None, false, None, None, false,
        )
        .await
        .unwrap();
        let state = Arc::new(AppState::for_test(db.clone()));

        apply_refund(&state, &bound("gold", None, "pay_1"))
            .await
            .unwrap();

        assert!(queued(&db).await.is_empty());
    }

    #[tokio::test]
    async fn the_lapse_sweep_queues_a_window_only_once_it_is_past_the_grace() {
        use crate::entitlement_lapse::{PAID_WINDOW_LAPSE_GRACE_SECS, sweep_once};
        let now = now_epoch_secs();
        // BUYER: ended a minute beyond the grace — queued.
        let (db, state) = roster_fixture(Some(now - PAID_WINDOW_LAPSE_GRACE_SECS - 60)).await;
        // Inside the grace by an hour — left standing for a late renewal.
        let inside: [u8; 32] = [0x7C; 32];
        db.add_subscriber(&AUTHOR, &inside, "gold", None)
            .await
            .unwrap();
        db.set_subscriber_valid_until(
            &AUTHOR,
            &inside,
            "gold",
            Some(now - PAID_WINDOW_LAPSE_GRACE_SECS + 3_600),
        )
        .await
        .unwrap();
        // No window at all (an author-approved subscriber) — never a lapse.
        let unwindowed: [u8; 32] = [0x7D; 32];
        db.add_subscriber(&AUTHOR, &unwindowed, "gold", None)
            .await
            .unwrap();
        // A membership tier long past its window — Rail C's, never a removal.
        seed_free_quota_tier(&db).await;
        db.create_subscription_tier(
            &AUTHOR, "club", 2, None, None, None, false, None, None, false,
        )
        .await
        .unwrap();
        db.upsert_membership_tier(&AUTHOR, "club", "free", "free")
            .await
            .unwrap();
        db.add_subscriber(&AUTHOR, &BUYER, "club", None)
            .await
            .unwrap();
        db.set_subscriber_valid_until(&AUTHOR, &BUYER, "club", Some(now - 30 * 86_400))
            .await
            .unwrap();

        let swept = sweep_once(&state).await.unwrap();

        assert_eq!(swept.removals_queued, 1);
        assert_eq!(queued(&db).await, vec![removal_of_buyer()]);
        // The next tick finds the removal already queued.
        assert_eq!(sweep_once(&state).await.unwrap().removals_queued, 0);
        assert_eq!(queued(&db).await.len(), 1);
    }

    #[tokio::test]
    async fn a_renewal_inside_the_grace_re_stamps_the_standing_row_in_place() {
        let (db, state) = roster_fixture(Some(now_epoch_secs() - 3_600)).await;
        let until = (now_epoch_secs() + 30 * 86_400) as u64;

        let applied = apply_payment(&state, &bound("gold", Some(until), "pay_renew"))
            .await
            .unwrap();

        assert_eq!(applied, PaymentApplied::Granted { queued: false });
        assert!(queued(&db).await.is_empty(), "a renewal is not a request");
        assert_eq!(
            db.get_subscriber_valid_until(&AUTHOR, &BUYER, "gold")
                .await
                .unwrap(),
            Some(until as i64)
        );
        assert!(db.is_subscriber(&AUTHOR, &BUYER, "gold").await.unwrap());
    }

    #[tokio::test]
    async fn a_payment_after_the_removal_is_queued_takes_the_request_path() {
        let (db, state) = roster_fixture(Some(now_epoch_secs() + 86_400)).await;
        apply_refund(&state, &bound("gold", None, "pay_1"))
            .await
            .unwrap();
        let until = (now_epoch_secs() + 30 * 86_400) as u64;

        let applied = apply_payment(&state, &bound("gold", Some(until), "pay_2"))
            .await
            .unwrap();

        assert_eq!(applied, PaymentApplied::Granted { queued: true });
        let mut kinds: Vec<String> = queued(&db).await.into_iter().map(|(k, _, _)| k).collect();
        kinds.sort();
        assert_eq!(kinds, vec!["subscribe", "unsubscribe"]);
        assert!(
            !db.is_subscriber(&AUTHOR, &BUYER, "gold").await.unwrap(),
            "the voided window is not silently revived on a row being removed"
        );
    }
}
