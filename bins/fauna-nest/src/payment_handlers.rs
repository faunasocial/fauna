//! WS-RPC handlers for the `fauna.payments.*` namespace — Pillar 3 of
//! `docs/goal/behavior/monetization.md`.
//!
//! - `providers.{set,list,remove}` — the creator's provider config (kind +
//!   webhook-verification secret + tier mapping), keyed on the bearer. The
//!   config surface is the client, per the product invariant; the nest holds
//!   only the verify secret.
//! - `claims.redeem` — bind a post-payment claim code to the bearer actor;
//!   the entitlement lands through `payment_core` (both rails).
//! - `claims.mint` — the author mints a code manually for a no-API provider
//!   (bank transfer, cash, …); `claims.list` — the author's own codes across
//!   both mint paths, the audit surface for webhook-minted codes too.
//!
//! Webhook ingress is NOT here (providers cannot speak WS-RPC) — see
//! `payment_routes`.

use std::time::Duration;

use fauna_core::data::Timestamp;
use fauna_core::feature_gate::{
    GateOp, GatedFeature, SURFACE_PAYMENTS_CLAIM_MINT, SURFACE_PAYMENTS_CLAIM_REDEEM,
    SURFACE_PAYMENTS_PROVIDER_CONFIGURE,
};
use fauna_core::identity::ActorId;
use fauna_protocol::payments::{
    ClaimItem, ClaimMintReply, ClaimMintRequest, ClaimRedeemReply, ClaimRedeemRequest,
    ClaimsListReply, ClaimsListRequest, ProviderItem, ProviderRemoveReply, ProviderRemoveRequest,
    ProviderSetReply, ProviderSetRequest, ProvidersListReply, ProvidersListRequest,
};
use fauna_protocol::{RpcError, Value, decode_strict as decode};

use crate::payment_core::{self, GrantError, RedeemError};
use crate::rpc_errors::encode_reply;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Error helpers (mirroring subscription_handlers' vocabulary) ──

fn malformed(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::malformed_ns("payments", reason)
}

fn unknown_provider(reason: &str) -> RpcError {
    let mut e = RpcError::new(
        "fauna.payments.unknown_provider",
        "error.payments.unknown_provider",
    );
    e.details = Some(Box::new(Value::String(reason.into())));
    e
}

fn tier_not_found(reason: &str) -> RpcError {
    crate::rpc_errors::tier_not_found_ns("payments", reason)
}

fn claim_not_found() -> RpcError {
    RpcError::new(
        "fauna.payments.claim_not_found",
        "error.payments.claim_not_found",
    )
}

fn claim_already_redeemed() -> RpcError {
    RpcError::new(
        "fauna.payments.claim_already_redeemed",
        "error.payments.claim_already_redeemed",
    )
}

fn claim_voided() -> RpcError {
    RpcError::new("fauna.payments.claim_voided", "error.payments.claim_voided")
}

use crate::rpc_errors::internal;

/// Map a write DB error: a SQLite `UNIQUE` violation (the freshly-generated
/// code collided) → a typed conflict the caller re-mints against, anything
/// else → internal. The full error chain (`{e:#}`) is matched because
/// `CacheDb` writers wrap the rusqlite error in `.context(...)`, which hides
/// the `UNIQUE` marker from `to_string()` (mirrors `admin_ws_handlers`'s
/// `unique_conflict`).
fn claim_code_conflict(e: anyhow::Error) -> RpcError {
    if format!("{e:#}").contains("UNIQUE") {
        RpcError::new(
            "fauna.payments.claim_conflict",
            "error.payments.claim_conflict",
        )
    } else {
        internal(e)
    }
}

fn grant_error(e: GrantError) -> RpcError {
    match e {
        GrantError::TierNotFound => tier_not_found("mapped tier no longer exists"),
        GrantError::Internal(why) => internal(why),
    }
}

// ── Handlers ───────────────────────────────────────────────────

/// `fauna.payments.providers.set` — upsert the bearer's config for one
/// provider kind. Rejects unknown adapter kinds, empty secrets, and a tier
/// mapping pointing at no existing tier (configure the tier first).
///
/// **Gate surface `payments.provider.configure`** — the sell side's first door
/// (`dynamic-features.md` § Charter members). Enabling a payment rail at all is
/// the operation a rule-setter denies when it denies the plane, which is why the
/// gate sits on `set` and deliberately **not** on `providers.remove`: every tier
/// can only tighten, so a deny that also froze a creator's config *in place*
/// would turn a restriction into a trap. Turning a rail off is never gated.
fn providers_set_handler() -> RpcHandler {
    Box::new(|state, author_id, payload| {
        Box::pin(async move {
            let req: ProviderSetRequest = decode(&payload).map_err(malformed)?;

            if fauna_payments::provider_for_kind(&req.kind).is_none() {
                return Err(unknown_provider(&format!(
                    "unknown provider kind {:?} (known: {})",
                    req.kind,
                    fauna_payments::known_kinds().join(", ")
                )));
            }
            if req.webhook_secret.is_empty() {
                return Err(malformed("webhook_secret must not be empty"));
            }
            if state
                .db
                .get_subscription_tier(&author_id, &req.tier)
                .await
                .map_err(internal)?
                .is_none()
            {
                return Err(tier_not_found("tier mapping points at no existing tier"));
            }

            // Gate AFTER the argument checks, for the reason `claims.redeem`
            // spells out: a request that was never a valid operation must not
            // spend the caller's quota. It is a plain sell-side authoring act —
            // it introduces no counterparty (a provider is a rail, not a person
            // the account transacts with) and moves no value, so both deltas are
            // honestly `0` rather than unset.
            crate::feature_gate::gate(
                &state,
                &author_id,
                &GateOp {
                    feature: GatedFeature::Payments,
                    surface: SURFACE_PAYMENTS_PROVIDER_CONFIGURE,
                    new_counterparties: 0,
                    magnitude: 0,
                },
            )
            .await?;

            state
                .db
                .upsert_payment_provider(&author_id, &req.kind, &req.webhook_secret, &req.tier)
                .await
                .map_err(internal)?;

            encode_reply(&ProviderSetReply {
                saved: true,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.payments.providers.list` — the bearer's configured providers.
/// Never echoes the webhook secret (least exposure — the form re-enters it).
fn providers_list_handler() -> RpcHandler {
    Box::new(|state, author_id, payload| {
        Box::pin(async move {
            let _req: ProvidersListRequest = decode(&payload).map_err(malformed)?;

            let rows = state
                .db
                .list_payment_providers(&author_id)
                .await
                .map_err(internal)?;

            let providers = rows
                .into_iter()
                .map(|p| ProviderItem {
                    kind: p.kind,
                    tier: p.tier_name,
                    // PaymentProviderRow.created_at is epoch seconds; Timestamp is micros.
                    created_at: Timestamp((p.created_at as u64).saturating_mul(1_000_000)),
                    last_verified_at: p.last_verified_at.map(|s| s as u64),
                    last_rejected_at: p.last_rejected_at.map(|s| s as u64),
                    extra: Default::default(),
                })
                .collect();

            encode_reply(&ProvidersListReply {
                providers,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.payments.providers.remove` — delete the bearer's config for one
/// provider kind. Already-granted entitlements and minted claim codes are
/// untouched (they answer to `valid_until`, not to config presence); only
/// future webhooks stop verifying (404 — no config row).
fn providers_remove_handler() -> RpcHandler {
    Box::new(|state, author_id, payload| {
        Box::pin(async move {
            let req: ProviderRemoveRequest = decode(&payload).map_err(malformed)?;

            let removed = state
                .db
                .remove_payment_provider(&author_id, &req.kind)
                .await
                .map_err(internal)?;

            encode_reply(&ProviderRemoveReply {
                removed,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.payments.claims.redeem` — bind a post-payment claim code to the
/// bearer; the entitlement lands like a verified bound-buyer payment.
///
/// **The first wired gate surface of the controversial-class feature plane**
/// (`dynamic-features.md` § Evaluation points; registry identifier
/// `payments.claim.redeem`). It is nest-arbitrated, so the gate is the floor —
/// it binds any client that never read `fauna.features.status`.
fn claims_redeem_handler() -> RpcHandler {
    Box::new(|state, redeemer_id, payload| {
        Box::pin(async move {
            let req: ClaimRedeemRequest = decode(&payload).map_err(malformed)?;
            let code = req.code.trim();
            if code.is_empty() {
                return Err(malformed("code must not be empty"));
            }

            // Resolve the claim's author BEFORE the gate, for two reasons that
            // both matter. (1) The counterparty this operation may newly
            // introduce is that author, and the gate needs the newness delta,
            // not an identity. (2) A code that does not exist must not spend the
            // caller's quota: a typo is not an operation, and charging for one
            // would let a fat-fingered client lock itself out of a plane it
            // never actually used. `redeem_claim` below re-reads and remains the
            // authority on redeemability — this read is not a second gate, and a
            // claim that races to redeemed between the two is still refused
            // there.
            let claim = state
                .db
                .get_payment_claim(code)
                .await
                .map_err(internal)?
                .ok_or_else(claim_not_found)?;
            let claim_author: [u8; 32] = claim
                .author_id
                .as_slice()
                .try_into()
                .map_err(|_| internal("payment_claim_codes.author_id is not 32 bytes"))?;

            // "Not currently in the feature's own records ⇒ new"
            // (§ The quota grammar's third refinement). The records here are the
            // redeemer's own live subscriptions: an author they already hold an
            // entitlement to is a standing counterparty and costs nothing, which
            // is the spouses-control direction. The count itself is NEVER read
            // back from these rows — they decide only whether this operation
            // introduces someone, and the bucket keeps what it was told, so an
            // unsubscribe cannot refund the unit it already spent.
            let standing = !state
                .db
                .get_subscribed_tiers(&claim_author, &redeemer_id)
                .await
                .map_err(internal)?
                .is_empty();

            crate::feature_gate::gate(
                &state,
                &redeemer_id,
                &GateOp {
                    feature: GatedFeature::Payments,
                    surface: SURFACE_PAYMENTS_CLAIM_REDEEM,
                    new_counterparties: u64::from(!standing),
                    // A claim code carries no amount — redemption grants an
                    // entitlement window, it does not move value. `0` is the
                    // honest magnitude, not an unset one.
                    magnitude: 0,
                },
            )
            .await?;

            let redeemed = payment_core::redeem_claim(&state, &redeemer_id, code)
                .await
                .map_err(|e| match e {
                    RedeemError::NotFound => claim_not_found(),
                    RedeemError::AlreadyRedeemed => claim_already_redeemed(),
                    RedeemError::Voided => claim_voided(),
                    RedeemError::Grant(g) => grant_error(g),
                })?;

            encode_reply(&ClaimRedeemReply {
                author: ActorId(redeemed.author_id),
                tier: redeemed.tier,
                // valid_until is epoch seconds in storage; micros on the wire.
                valid_until: redeemed
                    .valid_until
                    .map(|s| Timestamp((s as u64).saturating_mul(1_000_000))),
                queued: redeemed.queued,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.payments.claims.mint` — the author mints a claim code manually,
/// for a no-API provider (bank transfer, cash, …) already paid out-of-band
/// (monetization.md § Pillar 3: "the creator mints them manually"). Always
/// `provider = "manual"` — no adapter config is consulted, so this bypasses
/// `providers.set` entirely and never verifies a webhook.
///
/// **Gate surface `payments.claim.mint`** — the sell side's last door
/// (`dynamic-features.md` § Charter members). It is the one payments operation
/// that reaches nothing external at all, which is exactly why it needs the
/// floor: a client looping here mints bearer entitlements at whatever rate it
/// likes, and the tier-1 ceiling ("a compromised account or a looping client
/// minting claims") names this surface by name.
fn claims_mint_handler() -> RpcHandler {
    Box::new(|state, author_id, payload| {
        Box::pin(async move {
            let req: ClaimMintRequest = decode(&payload).map_err(malformed)?;

            // Hidden reads as nonexistent to every buy-side door (ruling 4,
            // `monetization.md` § The unifying model → *A tier may be hidden*):
            // a claim for a hidden tier could never be redeemed, so it is not
            // minted either.
            if !state
                .db
                .get_subscription_tier(&author_id, &req.tier)
                .await
                .map_err(internal)?
                .is_some_and(|t| !t.hidden)
            {
                return Err(tier_not_found("tier mapping points at no existing tier"));
            }

            // A minted claim is a bearer code: nobody holds it yet, so it
            // introduces no counterparty — the redeemer is counted on *their*
            // account at `payments.claim.redeem`, where an identity finally
            // exists. And a claim carries no amount (the same honest `0` the
            // redeem side documents), so this operation moves no value.
            crate::feature_gate::gate(
                &state,
                &author_id,
                &GateOp {
                    feature: GatedFeature::Payments,
                    surface: SURFACE_PAYMENTS_CLAIM_MINT,
                    new_counterparties: 0,
                    magnitude: 0,
                },
            )
            .await?;

            let code = crate::admin::generate_invite_code();
            // Wire valid_until is micros; storage is epoch seconds.
            let valid_until = req.valid_until.map(|t| (t.0 / 1_000_000) as i64);

            state
                .db
                .insert_payment_claim(
                    &code,
                    &author_id,
                    &req.tier,
                    "manual",
                    &format!("manual_{code}"),
                    valid_until,
                )
                .await
                .map_err(claim_code_conflict)?;

            encode_reply(&ClaimMintReply {
                code,
                tier: req.tier,
                valid_until: req.valid_until,
                extra: Default::default(),
            })
        })
    })
}

/// `fauna.payments.claims.list` — the bearer's own claim codes, both
/// manually-minted (`claims.mint`) and webhook-minted — the audit surface
/// for the latter, whose only other delivery channel is the webhook HTTP
/// response body.
fn claims_list_handler() -> RpcHandler {
    Box::new(|state, author_id, payload| {
        Box::pin(async move {
            let _req: ClaimsListRequest = decode(&payload).map_err(malformed)?;

            let rows = state
                .db
                .list_payment_claims(&author_id)
                .await
                .map_err(internal)?;

            let mut claims = Vec::with_capacity(rows.len());
            for c in rows {
                let redeemed_by = match c.redeemed_by {
                    Some(b) => {
                        let arr: [u8; 32] = b
                            .as_slice()
                            .try_into()
                            .map_err(|_| internal("redeemed_by is not 32 bytes"))?;
                        Some(ActorId(arr))
                    }
                    None => None,
                };
                claims.push(ClaimItem {
                    code: c.code,
                    tier: c.tier_name,
                    provider: c.provider,
                    // PaymentClaimRow timestamps are epoch seconds; Timestamp is micros.
                    valid_until: c
                        .valid_until
                        .map(|s| Timestamp((s as u64).saturating_mul(1_000_000))),
                    created_at: Timestamp((c.created_at as u64).saturating_mul(1_000_000)),
                    redeemed_by,
                    redeemed_at: c
                        .redeemed_at
                        .map(|s| Timestamp((s as u64).saturating_mul(1_000_000))),
                    voided_at: c
                        .voided_at
                        .map(|s| Timestamp((s as u64).saturating_mul(1_000_000))),
                    extra: Default::default(),
                });
            }

            encode_reply(&ClaimsListReply {
                claims,
                extra: Default::default(),
            })
        })
    })
}

/// Register all fauna.payments.* WS-RPC handlers on the builder.
pub fn register_payment_handlers(b: &mut RpcRouterBuilder) {
    // `forbid_replay: false` audited 2026-08-01 (the money family's pass; the
    // criterion and its consequence are stated once in
    // `register_subscription_handlers`). Pure upsert keyed on
    // `(author_id, kind)` (`upsert_payment_provider`) behind three read-only
    // validations, so a repeat overwrites the identical row. No secret is
    // rotated as a side effect: the webhook secret written is the one the
    // request carries.
    b.add(
        "fauna.payments.providers.set",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: providers_set_handler(),
        },
    );
    b.add(
        "fauna.payments.providers.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: providers_list_handler(),
        },
    );
    // `forbid_replay: false` audited 2026-08-01. Keyed delete on
    // `(author_id, kind)`; a repeat matches nothing and the reply is honest
    // about it — `removed` is the row count, and this kind reports
    // `removed: false` rather than erroring, so unlike the delete-shaped kinds
    // in `register_subscription_handlers` its *reply* survives a replay too.
    // Deliberately narrow: already-granted entitlements and minted claim codes
    // outlive the config row (they answer to `valid_until`), so nothing a
    // repeat could re-delete hangs off this.
    b.add(
        "fauna.payments.providers.remove",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: providers_remove_handler(),
        },
    );
    // `forbid_replay: false` audited 2026-08-01 — the money path, so audited
    // to the write rather than to the doc comment. `redeem_claim` is idempotent
    // *by construction, for the same actor*: a claim already redeemed by a
    // different actor is refused (`AlreadyRedeemed`), and a same-actor repeat
    // skips the `redeemed_at` stamp and only re-applies the grant
    // (`payment_core.rs`). Every write the re-applied grant reaches is keyed:
    // `set_subscriber_valid_until` (same window), `upsert_payment_entitled_request`
    // and `enqueue_unlock_fanout` (both `ON CONFLICT(author_id, subscriber_id,
    // tier_name, kind) DO UPDATE`). So a replay cannot mint a second
    // entitlement, re-charge anything, or double-enqueue an author-client mint.
    // The one thing a repeat does NOT do is re-run the claim-minting arm of
    // `apply_payment` — the buyer is bound to the redeemer before the call, so
    // that arm is unreachable from here.
    b.add(
        "fauna.payments.claims.redeem",
        RpcKindMeta {
            forbid_replay: false,
            // Plaintext redemption runs the approve cascade (KeyBlob regen per
            // affected tier) — same headroom as requests.approve.
            default_deadline: Duration::from_secs(15),
            handler: claims_redeem_handler(),
        },
    );
    b.add(
        "fauna.payments.claims.mint",
        RpcKindMeta {
            // NOT idempotent: each call allocates a fresh random code
            // (`admin::generate_invite_code`) and inserts a new row keyed on
            // it, with the external reference derived from that same code — so
            // nothing dedups, and a replayed mint leaves a second
            // independently redeemable credential against one out-of-band
            // payment. `transport.md` § Idempotency and reconnect-with-resume
            // requires `true` for exactly this shape (the idempotency cache is
            // per-connection and cannot catch a retry after a reconnect).
            // Standing evidence:
            // `conformance_payments::minting_twice_yields_two_independently_redeemable_claims`.
            // Mirror any change in `KindRegistry::register_payments_kinds`.
            forbid_replay: true,
            default_deadline: Duration::from_secs(5),
            handler: claims_mint_handler(),
        },
    );
    b.add(
        "fauna.payments.claims.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: claims_list_handler(),
        },
    );
}
