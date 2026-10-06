//! WS-RPC request/reply types for the `fauna.payments.*` namespace —
//! Pillar 3 of `docs/goal/behavior/monetization.md` (payment-provider API).
//!
//! Two small families:
//!
//! - `fauna.payments.providers.{set,list,remove}` — the creator configures
//!   a payment provider from their client (kind + webhook-verification
//!   secret + tier mapping), persisted in nest state. The nest holds ONLY
//!   the verify secret; payout credentials never touch it.
//! - `fauna.payments.claims.redeem` — the buyer pastes a post-payment claim
//!   code; the entitlement binds to the redeeming (bearer) actor and lands
//!   through Pillar 1's grant machinery.
//!
//! Webhook ingress is deliberately NOT here: payment providers cannot speak
//! WS-RPC, so it lands on the nest's public HTTP surface (a reserved
//! `/api/v1/…` path — same class as the surviving public HTTP reads).

use crate::Value;
use fauna_core::data::Timestamp;
use fauna_core::identity::ActorId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ── providers.set ──────────────────────────────────────────────

/// Upsert one provider config for the calling author (keyed on `kind` —
/// one config per provider kind per author, first cut).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderSetRequest {
    /// Provider kind discriminant (`"fake"`, `"stripe"`, …).
    pub kind: String,
    /// Webhook-verification secret — verify-only (checks webhook
    /// signatures; can never move money or read payout accounts).
    pub webhook_secret: String,
    /// The tier a verified payment entitles (first cut: one mapping).
    pub tier: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderSetReply {
    pub saved: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── providers.list ─────────────────────────────────────────────

/// Authenticated read of the calling author's own provider configs. Zero
/// args — the nest keys on the bearer (mirrors `TiersListRequest`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProvidersListRequest {}

/// One configured provider as the author sees it. Deliberately does NOT
/// echo `webhook_secret` back — the row renders kind + mapping; changing
/// the secret means re-entering it in the form (least exposure).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderItem {
    pub kind: String,
    pub tier: String,
    pub created_at: Timestamp,
    /// Evidence-based provider-status stamps (monetization.md § Pillar 3 →
    /// "Provider status — evidence-based, no ping", ratified 2026-07-16) —
    /// epoch **seconds**, plain optional (not nested under `created_at`'s
    /// microsecond `Timestamp` convention, and not wrapped further — the
    /// dag-cbor wire forbids nested `Option`). `None`/`None` → `configured`;
    /// `last_rejected_at >= last_verified_at` → `error`; else `verified`
    /// (`fauna_core::format::provider_status_label`). Additive: a configured provider never verified or
    /// rejected carries neither stamp, so both decode `None`.
    #[serde(default)]
    pub last_verified_at: Option<u64>,
    #[serde(default)]
    pub last_rejected_at: Option<u64>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProvidersListReply {
    /// The author's configured providers, ascending by `kind`.
    pub providers: Vec<ProviderItem>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── providers.remove ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderRemoveRequest {
    pub kind: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderRemoveReply {
    pub removed: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── claims.redeem ──────────────────────────────────────────────

/// Redeem a post-payment claim code; the entitlement binds to the bearer
/// actor (monetization.md § Pillar 3 Q4 — the universal fallback binding).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimRedeemRequest {
    pub code: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimRedeemReply {
    /// The creator whose tier the claim entitles.
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub author: ActorId,
    pub tier: String,
    /// End of the paid window (micros, like every wire timestamp); absent =
    /// no expiry until a refund voids it.
    #[serde(default)]
    pub valid_until: Option<Timestamp>,
    /// `true` → the grant enqueued for the creator's client to mint the
    /// KeyBlob (client-minted tier, the default — same queue subscribe
    /// uses); `false` → the subscription is already active (a window extension or a
    /// membership grant).
    pub queued: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── claims.mint ─────────────────────────────────────────────────

/// The author mints a claim code manually, for a no-API provider (bank
/// transfer, cash, …) they've already been paid by out-of-band
/// (monetization.md § Pillar 3: "the creator mints them manually"). No
/// provider config is consulted — the minted claim's `provider` is always
/// `"manual"` and never verifies a webhook.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimMintRequest {
    /// The tier this code entitles — must be one of the author's own tiers.
    pub tier: String,
    /// End of the paid window (micros); absent = no expiry until the author
    /// voids it.
    #[serde(default)]
    pub valid_until: Option<Timestamp>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimMintReply {
    /// The freshly minted code — the invite-code alphabet (10 unambiguous
    /// uppercase chars); hand it to the buyer out-of-band.
    pub code: String,
    pub tier: String,
    #[serde(default)]
    pub valid_until: Option<Timestamp>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── claims.list ─────────────────────────────────────────────────

/// Authenticated read of the calling author's own claim codes — the audit
/// surface for BOTH manually-minted (`claims.mint`) and webhook-minted
/// codes, whose only other delivery channel is the webhook HTTP response
/// body. Zero args — the nest keys on the bearer (mirrors
/// `ProvidersListRequest`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ClaimsListRequest {}

/// One claim code as the author sees it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimItem {
    pub code: String,
    pub tier: String,
    /// `"manual"` for an author-minted code, else the provider kind that
    /// minted it (`"fake"`, `"stripe"`, …).
    pub provider: String,
    #[serde(default)]
    pub valid_until: Option<Timestamp>,
    pub created_at: Timestamp,
    /// Who redeemed it, if anyone — absent means still unredeemed.
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    #[serde(default)]
    pub redeemed_by: Option<ActorId>,
    #[serde(default)]
    pub redeemed_at: Option<Timestamp>,
    /// Set when a refund/dispute voided an unredeemed claim.
    #[serde(default)]
    pub voided_at: Option<Timestamp>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ClaimsListReply {
    /// The author's claim codes, newest first.
    pub claims: Vec<ClaimItem>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}
