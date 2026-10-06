//! UniFFI façade for the `fauna.payments.*` Layer-3 WS-RPC kinds — Pillar 3
//! of `docs/goal/behavior/monetization.md`: the author-side payment-provider
//! configuration (profile Tiers-tab §4 provider section) and the buyer-side
//! claim-code redemption (`subscription-settings` page).
//!
//! [`FfiPaymentsClient`] wraps `fauna_client_payments::PaymentsClient` (which
//! in turn wraps the shared `NestClient`); the mirror records below are the
//! FFI-visible shape of `fauna_protocol::payments::*`. The Rust-native Linux
//! app calls the same `PaymentsClient` directly — this seam gives Apple /
//! Windows / Android the identical surface over UniFFI.
//!
//! `ActorId` crosses as `Vec<u8>` (32 bytes) and `Timestamp` as `u64`
//! (microseconds), matching `subscriptions_client.rs`. All conversions are
//! total field maps: adding a field to the protocol types is a compile error
//! here, so the mirror can't silently drift (priority #1/#4).

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_payments::PaymentsClient;
use fauna_client_payments::payments::{ClaimItem, ClaimMintReply, ClaimRedeemReply, ProviderItem};

use crate::{FfiError, stringify};

// ── record mirrors ─────────────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::payments::ProviderItem`] — one configured
/// provider as the author sees it. Deliberately carries no webhook secret
/// (the list reply omits it; changing it means re-entering it in the form).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiProviderItem {
    pub kind: String,
    pub tier: String,
    /// Config creation time in microseconds since the Unix epoch.
    pub created_at: u64,
    /// Evidence-based provider-status stamps (monetization.md § Pillar 3 →
    /// "Provider status — evidence-based, no ping"), epoch **seconds**
    /// (unlike `created_at`'s microseconds). `None`/`None` = no webhook
    /// delivery seen yet. Feed both into `value_format::provider_status_label`
    /// for the §4 status badge — never re-derive the branch per-app.
    pub last_verified_at: Option<u64>,
    pub last_rejected_at: Option<u64>,
}

impl From<ProviderItem> for FfiProviderItem {
    fn from(p: ProviderItem) -> Self {
        FfiProviderItem {
            kind: p.kind,
            tier: p.tier,
            created_at: p.created_at.0,
            last_verified_at: p.last_verified_at,
            last_rejected_at: p.last_rejected_at,
        }
    }
}

/// FFI mirror of [`fauna_protocol::payments::ClaimRedeemReply`] — the
/// entitlement a redeemed claim code binds to the calling actor.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiClaimRedeemReply {
    /// The creator whose tier the claim entitles (32-byte `ActorId`).
    pub author: Vec<u8>,
    pub tier: String,
    /// End of the paid window in microseconds since the Unix epoch; `None` =
    /// no expiry until a refund voids it.
    pub valid_until: Option<u64>,
    /// `true` → the grant enqueued for the creator's client to mint the
    /// KeyBlob (client-minted tier, the default — same queue subscribe
    /// uses); `false` → already active (a window
    /// extension).
    pub queued: bool,
}

impl From<ClaimRedeemReply> for FfiClaimRedeemReply {
    fn from(r: ClaimRedeemReply) -> Self {
        FfiClaimRedeemReply {
            author: r.author.0.to_vec(),
            tier: r.tier,
            valid_until: r.valid_until.map(|t| t.0),
            queued: r.queued,
        }
    }
}

/// FFI mirror of [`fauna_protocol::payments::ClaimMintReply`] — the freshly
/// minted claim code (author-side, manual/no-API providers).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiClaimMintReply {
    pub code: String,
    pub tier: String,
    /// End of the paid window in microseconds since the Unix epoch; `None` =
    /// no expiry until the author voids it.
    pub valid_until: Option<u64>,
}

impl From<ClaimMintReply> for FfiClaimMintReply {
    fn from(r: ClaimMintReply) -> Self {
        FfiClaimMintReply {
            code: r.code,
            tier: r.tier,
            valid_until: r.valid_until.map(|t| t.0),
        }
    }
}

/// FFI mirror of [`fauna_protocol::payments::ClaimItem`] — one claim code as
/// the author sees it (the audit surface for BOTH manually- and
/// webhook-minted codes).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiClaimItem {
    pub code: String,
    pub tier: String,
    /// `"manual"` for an author-minted code, else the provider kind that
    /// minted it (`"fake"`, `"stripe"`, …).
    pub provider: String,
    pub valid_until: Option<u64>,
    pub created_at: u64,
    /// Who redeemed it, if anyone — absent means still unredeemed (32-byte
    /// `ActorId`).
    pub redeemed_by: Option<Vec<u8>>,
    pub redeemed_at: Option<u64>,
    /// Set when a refund/dispute voided an unredeemed claim.
    pub voided_at: Option<u64>,
}

impl From<ClaimItem> for FfiClaimItem {
    fn from(c: ClaimItem) -> Self {
        FfiClaimItem {
            code: c.code,
            tier: c.tier,
            provider: c.provider,
            valid_until: c.valid_until.map(|t| t.0),
            created_at: c.created_at.0,
            redeemed_by: c.redeemed_by.map(|a| a.0.to_vec()),
            redeemed_at: c.redeemed_at.map(|t| t.0),
            voided_at: c.voided_at.map(|t| t.0),
        }
    }
}

/// Every registered payment-provider kind — the provider form's kind select
/// enumerates these. Re-exported from the `fauna-payments` adapter registry,
/// the same list the nest's `providers.set` validates against.
#[uniffi::export]
pub fn payments_known_kinds() -> Vec<String> {
    fauna_client_payments::known_kinds()
        .iter()
        .map(|k| k.to_string())
        .collect()
}

/// The exact URL a creator registers at their provider's dashboard for
/// `kind` — the §4 provider form previews it live as the kind select
/// changes. Derived from the same constant the nest registers its ingress
/// route from, so a client can never hand-assemble a stale path.
#[uniffi::export]
pub fn payments_webhook_url(base_url: String, author_id_hex: String, kind: String) -> String {
    fauna_client_payments::webhook_url(&base_url, &author_id_hex, &kind)
}

// ── FfiPaymentsClient ──────────────────────────────────────────────────

/// UniFFI handle for the `fauna.payments.*` kinds. Construct via
/// [`crate::nest_client::FfiNestClient::payments`]; methods are exposed to
/// Swift as `async throws` and Kotlin as `suspend fun`.
#[derive(uniffi::Object)]
pub struct FfiPaymentsClient {
    nest: Arc<NestClient>,
}

impl FfiPaymentsClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> PaymentsClient<Arc<NestClient>> {
        PaymentsClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiPaymentsClient {
    /// `fauna.payments.providers.set` — upsert the calling author's config
    /// for one provider kind (kind + webhook-verification secret + entitled
    /// tier). Returns whether the config was saved. Typed errors:
    /// `fauna.payments.{unknown_provider,tier_not_found,malformed}`.
    pub async fn providers_set(
        &self,
        kind: String,
        webhook_secret: String,
        tier: String,
    ) -> Result<bool, FfiError> {
        self.client()
            .providers_set(kind, webhook_secret, tier)
            .await
            .map_err(stringify)
    }

    /// `fauna.payments.providers.list` — the calling author's own configured
    /// providers, ascending by kind; rows never carry the webhook secret.
    /// Replay-safe pure read.
    pub async fn providers_list(&self) -> Result<Vec<FfiProviderItem>, FfiError> {
        self.client()
            .providers_list()
            .await
            .map(|rows| rows.into_iter().map(FfiProviderItem::from).collect())
            .map_err(stringify)
    }

    /// `fauna.payments.providers.remove` — delete the calling author's
    /// config for one provider kind. Idempotent; returns whether a row was
    /// removed.
    pub async fn providers_remove(&self, kind: String) -> Result<bool, FfiError> {
        self.client()
            .providers_remove(kind)
            .await
            .map_err(stringify)
    }

    /// `fauna.payments.claims.redeem` — bind a post-payment claim code to
    /// the calling actor; the entitlement lands through Pillar 1's grant
    /// queue. Typed errors:
    /// `fauna.payments.claim_{not_found,already_redeemed,voided}`.
    pub async fn claims_redeem(&self, code: String) -> Result<FfiClaimRedeemReply, FfiError> {
        self.client()
            .claims_redeem(code)
            .await
            .map(FfiClaimRedeemReply::from)
            .map_err(stringify)
    }

    /// `fauna.payments.claims.mint` — the author mints a claim code manually,
    /// for a no-API provider (bank transfer, cash, …) already paid
    /// out-of-band. Always `provider = "manual"`; the nest rejects a `tier`
    /// that isn't one of the author's own tiers.
    pub async fn claims_mint(
        &self,
        tier: String,
        valid_until: Option<u64>,
    ) -> Result<FfiClaimMintReply, FfiError> {
        self.client()
            .claims_mint(tier, valid_until.map(fauna_core::data::Timestamp))
            .await
            .map(FfiClaimMintReply::from)
            .map_err(stringify)
    }

    /// `fauna.payments.claims.list` — the calling author's own claim codes,
    /// newest first; the audit surface for BOTH manually-minted and
    /// webhook-minted codes. Replay-safe pure read.
    pub async fn claims_list(&self) -> Result<Vec<FfiClaimItem>, FfiError> {
        self.client()
            .claims_list()
            .await
            .map(|rows| rows.into_iter().map(FfiClaimItem::from).collect())
            .map_err(stringify)
    }
}
