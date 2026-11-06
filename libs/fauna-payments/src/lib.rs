//! Payment-provider adapter seam — Pillar 3 of
//! `docs/goal/behavior/monetization.md`.
//!
//! A creator connects their own payment provider (Stripe, Ko-fi, BTCPay, …)
//! and "this person paid for tier X until Y" flows into a Fauna tier
//! entitlement — no Fauna-operated payment rail, no provider lock-in. This
//! crate owns the two provider-facing halves of that design:
//!
//! - The [`PaymentProvider`] adapter trait: **verify** an inbound webhook's
//!   authenticity (provider signature scheme over the exact raw body),
//!   **map** the payment to a tier (via the creator-configured
//!   [`TierMapping`]), and **parse** the validity window. All
//!   provider-specific knowledge (header names, JSON shapes, event-type
//!   vocabularies) is quarantined inside the per-provider modules
//!   ([`fake`], [`stripe`]); nothing downstream is provider-aware.
//! - The [`PaymentEntitlement`] narrow waist: the single normalized value a
//!   verified payment reduces to. One mechanism-agnostic entitlement engine
//!   (the nest's — riding Pillar 1's tier machinery) consumes it and drives
//!   both delivery rails. Webhooks are only the *first* mechanism: a claim
//!   code and (later) a NIP-57 zap receipt reduce to the very same value, and
//!   nothing downstream of it may special-case any of them
//!   (monetization.md § *One model, many mechanisms, two targets*).
//! - The [`tips`] module: the *other* consequence class of the same
//!   mechanisms. A tip names *(payee, post)* and grants nothing, so it
//!   deliberately does **not** pass the waist above (monetization.md
//!   § Tips) — it is a sibling value under the same mechanism-blindness
//!   discipline, not an entitlement with empty fields.
//!
//! Pure — no tokio, no clock, no I/O (`verify` takes `now_secs` from the
//! caller), wasm32-clean like the other shared client crates.

pub mod asking_price;
// The fake provider verifies with a secret its author chose and grants that
// author's own tiers, so it is test machinery, never a production surface: it
// is compiled into test and debug builds and into a release build only through
// the `test-helpers` feature an e2e flavor forwards
// (`e2e-automation-surface-gating.md` rule (a)) — a production build's
// registry has no `fake`. No `test` arm, on purpose: `cargo test --release`
// is then the production registry, which is how its pin runs.
#[cfg(any(debug_assertions, feature = "test-helpers"))]
pub mod fake;
pub mod stripe;
pub mod tips;

use fauna_core::identity::ActorId;
use std::collections::BTreeMap;

// ── Normalized event (adapter output) ──────────────────────────

/// What a verified provider event means for the entitlement engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentEventKind {
    /// A payment or renewal — grant the entitlement, or extend its
    /// `valid_until` window (expiry self-heals; monetization.md § Pillar 3).
    Payment,
    /// A refund or dispute — shorten/void the window. No revocation list
    /// exists anywhere; voiding `valid_until` is the whole mechanism.
    Refund,
    /// A signature-valid event of a type this integration doesn't act on
    /// (providers deliver many event types per endpoint). The ingress
    /// acknowledges it (2xx — it WAS authentic and delivered) and does
    /// nothing; failing instead would put the provider's delivery system
    /// into retry/disable loops.
    Ignored,
}

/// The normalized, authenticity-verified payment event an adapter's
/// [`PaymentProvider::verify`] yields. Provider-agnostic: field semantics are
/// identical whichever adapter produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPaymentEvent {
    pub kind: PaymentEventKind,
    /// The buyer↔actor binding carried through checkout
    /// (`client_reference_id = actor_id` hex — monetization.md § Pillar 3
    /// Q4). `None` → unbound: the engine mints a claim code instead.
    pub buyer_reference: Option<String>,
    /// The product the payment is for, as the adapter names it (Stripe: a
    /// Checkout Session's Payment Link id) — matched by
    /// [`TierMapping::covers`] when the creator narrowed the mapping to one
    /// product. `None` on an event that names none (a renewal invoice, a
    /// refund, a hand-built Checkout with no Payment Link).
    pub product_reference: Option<String>,
    /// End of the paid validity window, seconds since epoch (provider-native
    /// unit). `None` → the event carries no window (e.g. a one-off payment):
    /// the entitlement does not expire until a refund voids it.
    pub valid_until_secs: Option<u64>,
    /// Provider-side event/payment id — idempotency key (providers redeliver)
    /// and the audit link back to the provider dashboard.
    pub external_ref: String,
    /// The provider's post-checkout reference — the value its redirect hands
    /// the buyer's browser (Stripe: the Checkout Session id substituted for
    /// `{CHECKOUT_SESSION_ID}`). Persisted on a minted claim so the visitor
    /// return door ([`return_url`]) can show the buyer their code
    /// (monetization.md § Pillar 2 → *Visitor entitlement doors* ruling (2)).
    /// `None` on every event that is not a checkout completion.
    pub return_ref: Option<String>,
    /// The stable references of the PURCHASE this event belongs to (Stripe:
    /// the subscription id, the payment intent id, the Checkout Session id) —
    /// distinct from [`Self::external_ref`], which names one delivery and
    /// differs on every event. A payment records its claim or buyer binding
    /// against each of them; a later renewal extends, and a refund or dispute
    /// voids, whatever any one of them names (monetization.md § Expiry &
    /// revocation). Empty when the event names no purchase — the engine then
    /// falls back to the `external_ref` lookup.
    pub purchase_refs: Vec<String>,
}

/// Why an inbound webhook was rejected. Every variant MUST surface as a
/// non-2xx at the HTTP ingress — the web-serving catch-all answers unmatched
/// paths with a `200` info page, so only an explicit error status tells the
/// provider's delivery system the event was NOT accepted
/// (monetization.md § Pillar 3, webhook ingress).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// The provider's signature header is absent or unparseable.
    MissingSignature,
    /// Signature present but wrong for this body + secret.
    BadSignature,
    /// Signature timestamp outside the replay tolerance.
    StaleTimestamp,
    /// Authentic-looking envelope but a body the adapter cannot interpret.
    MalformedPayload(String),
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::MissingSignature => write!(f, "missing or unparseable signature"),
            VerifyError::BadSignature => write!(f, "signature verification failed"),
            VerifyError::StaleTimestamp => write!(f, "signature timestamp outside tolerance"),
            VerifyError::MalformedPayload(why) => write!(f, "malformed payload: {why}"),
        }
    }
}

impl std::error::Error for VerifyError {}

// ── Webhook headers (transport-agnostic view) ──────────────────

/// Case-insensitive header view handed to adapters, decoupling them from any
/// HTTP framework type. Keys are stored lower-cased.
#[derive(Debug, Clone, Default)]
pub struct WebhookHeaders(BTreeMap<String, String>);

impl WebhookHeaders {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from `(name, value)` pairs; names are lower-cased. Later
    /// duplicates win (webhook signature headers are single-valued in
    /// practice).
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: Into<String>,
    {
        Self(
            pairs
                .into_iter()
                .map(|(k, v)| (k.as_ref().to_ascii_lowercase(), v.into()))
                .collect(),
        )
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(&name.to_ascii_lowercase()).map(String::as_str)
    }
}

// ── The adapter trait ──────────────────────────────────────────

/// A pluggable third-party payment provider. Implementations quarantine all
/// provider knowledge; the entitlement engine only ever sees
/// [`VerifiedPaymentEvent`] / [`PaymentEntitlement`].
pub trait PaymentProvider: Send + Sync {
    /// Stable provider discriminant (`"fake"`, `"stripe"`, …) — the `kind`
    /// the creator picks in the provider form, the webhook path segment, and
    /// the `provider` field of the resulting entitlement.
    fn kind(&self) -> &'static str;

    /// Verify an inbound webhook's authenticity against the creator's
    /// configured webhook-verification `secret` and normalize it.
    ///
    /// `body` is the exact raw request bytes (signatures cover the bytes on
    /// the wire, never a re-serialization); `now_secs` is the caller's clock
    /// (seconds since epoch) for replay-tolerance checks — the trait itself
    /// stays clock-free.
    fn verify(
        &self,
        secret: &str,
        headers: &WebhookHeaders,
        body: &[u8],
        now_secs: u64,
    ) -> Result<VerifiedPaymentEvent, VerifyError>;
}

/// Adapter registry. `None` for an unknown kind — the ingress maps that to a
/// non-2xx (an unknown provider must not read as delivered).
pub fn provider_for_kind(kind: &str) -> Option<&'static dyn PaymentProvider> {
    match kind {
        #[cfg(any(debug_assertions, feature = "test-helpers"))]
        fake::KIND => Some(&fake::FakeProvider),
        stripe::KIND => Some(&stripe::StripeProvider),
        _ => None,
    }
}

/// Every registered provider kind (the client provider-form `kind` select
/// enumerates these). `fake` is listed only where it is compiled (see the
/// module gate above).
pub fn known_kinds() -> &'static [&'static str] {
    #[cfg(any(debug_assertions, feature = "test-helpers"))]
    {
        &[fake::KIND, stripe::KIND]
    }
    #[cfg(not(any(debug_assertions, feature = "test-helpers")))]
    {
        &[stripe::KIND]
    }
}

// ── Webhook ingress URL ────────────────────────────────────────

/// Path prefix of the nest's webhook-ingress route. Providers cannot speak
/// WS-RPC, so a verified payment arrives over plain HTTP on this reserved
/// `/api/v1/…` path (monetization.md § Pillar 3 — "Webhook ingress"); it is
/// never shadowable by user web content (`web-content-hosting.md` invariant
/// 3).
///
/// Owned here, next to the provider registry, because both sides of the
/// route need it and neither may drift: the nest registers its axum route
/// from this constant, and every app builds the creator-facing URL from
/// [`webhook_url`]. Same reasoning as [`known_kinds`] — client and nest can
/// never disagree at equal versions.
pub const WEBHOOK_PATH_PREFIX: &str = "/api/v1/payments/webhook";

/// The exact URL a creator registers at their provider's dashboard for
/// `kind`, so they never hand-assemble it:
/// `<base_url>/api/v1/payments/webhook/<author_id_hex>/<kind>`.
///
/// Derived entirely client-side from values the client already holds (its
/// nest base URL + the signed-in author + the selected kind) — no nest
/// round-trip, so a provider form can preview it live as the kind select
/// changes, before the provider is even saved.
///
/// `base_url`'s trailing slash is optional: a stored nest URL may carry one
/// and a double slash would not match the nest's route.
pub fn webhook_url(base_url: &str, author_id_hex: &str, kind: &str) -> String {
    format!(
        "{}{}/{}/{}",
        base_url.trim_end_matches('/'),
        WEBHOOK_PATH_PREFIX,
        author_id_hex,
        kind
    )
}

// ── Visitor entitlement doors ──────────────────────────────────

/// Path of the web paywall's redeem door: the target of the static claim-code
/// `<form>` on every paywall teaser (monetization.md § Pillar 2 → *Visitor
/// entitlement doors* ruling (1)). A reserved `/api/v1/…` path like
/// [`WEBHOOK_PATH_PREFIX`] — never shadowable by user web content — and
/// host-relative: the nest resolves the site owner from the request's `Host`,
/// so the one path serves every creator's site.
pub const REDEEM_PATH: &str = "/api/v1/paywall/redeem";

/// Path prefix of the web paywall's return door
/// (`<prefix>/<kind>?ref=<provider reference>`), where a provider's
/// post-checkout redirect lands the buyer (ruling (2)). The nest registers
/// its route from this constant and every app renders the creator-facing URL
/// from [`return_url`], so the two cannot drift.
pub const RETURN_PATH_PREFIX: &str = "/api/v1/paywall/return";

/// The placeholder `kind`'s checkout redirect substitutes with the reference
/// its webhook later reports as [`VerifiedPaymentEvent::return_ref`].
fn return_ref_placeholder(kind: &str) -> &'static str {
    match kind {
        stripe::KIND => "{CHECKOUT_SESSION_ID}",
        _ => "{REF}",
    }
}

/// The exact post-checkout redirect URL a creator pastes into `kind`'s
/// checkout settings: `<site_url>/api/v1/paywall/return/<kind>?ref=<placeholder>`.
///
/// `site_url` is the creator's **web site** origin (apex, subdomain or custom
/// domain), not the app's nest URL: the door resolves the site owner from the
/// request's `Host`, exactly as the serving path does. Its trailing slash is
/// optional, as for [`webhook_url`].
pub fn return_url(site_url: &str, kind: &str) -> String {
    format!(
        "{}{}/{}?ref={}",
        site_url.trim_end_matches('/'),
        RETURN_PATH_PREFIX,
        kind,
        return_ref_placeholder(kind)
    )
}

// ── Tier mapping ───────────────────────────────────────────────

/// The creator-configured payment→tier mapping ("map" capability of the
/// adapter seam): one tier per provider, optionally narrowed to the one
/// product the tier is sold as (monetization.md § Pillar 3 →
/// *Configuration*). With no product every verified payment on the
/// provider account lands on the tier; with one, a payment for anything
/// else the creator sells there (a tip link, another product) is not
/// covered ([`Self::covers`]) and mints nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierMapping {
    pub tier: String,
    /// The adapter's product reference the tier is sold as (Stripe: the
    /// Payment Link id, `plink_…` — a Checkout Session's webhook carries no
    /// line items, but names its Payment Link). `None` = every payment.
    pub product: Option<String>,
}

impl TierMapping {
    /// Resolve the tier a verified payment entitles. Total: whether the
    /// payment is for this tier at all is [`Self::covers`]'s question, asked
    /// first by the ingress.
    pub fn tier_for(&self, _product_reference: Option<&str>) -> Option<&str> {
        Some(&self.tier)
    }

    /// Whether this mapping covers `event`. Only a [`PaymentEventKind::Payment`]
    /// is ever withheld: a refund or dispute names a purchase, never a product,
    /// and voids only what that purchase recorded, so it needs no product
    /// check. A payment is covered when no product is configured, when it
    /// names the configured product, or when `recorded_product` — the product
    /// an earlier payment of the same purchase was recorded under — is the
    /// configured one (a subscription's renewal invoice names its
    /// subscription, not the Payment Link that sold it). A purchase recorded
    /// under another product, or under none, is not this tier's: re-pointing
    /// the mapping never moves an existing subscription onto the new product's
    /// tier. `None` = no purchase recorded, or one that named no product.
    pub fn covers(&self, event: &VerifiedPaymentEvent, recorded_product: Option<&str>) -> bool {
        let Some(product) = self.product.as_deref() else {
            return true;
        };
        event.kind != PaymentEventKind::Payment
            || event.product_reference.as_deref() == Some(product)
            || recorded_product == Some(product)
    }
}

// ── The narrow waist ───────────────────────────────────────────

/// Who a verified payment entitles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Buyer {
    /// Bound at checkout (`client_reference_id = actor_id`) or at claim-code
    /// redemption.
    Actor(ActorId),
    /// No binding reference — the engine mints a post-payment claim code the
    /// buyer redeems later (in their Fauna app → binds their actor; on
    /// the paywall page → Rail B only).
    Unbound,
}

/// The normalized entitlement every verified payment reduces to — the narrow
/// waist between the payment mechanisms and Pillar 1's tier machinery
/// (monetization.md § Pillar 3 + § *One model, many mechanisms, two targets*).
/// Nothing downstream of this type is mechanism-aware.
///
/// It names a **(payee, tier)** pair: the payee is the user being paid (a
/// creator, or a nest admin selling membership — "admin is a user with an extra
/// role"), and the *tier's own designation* — not anything on this value —
/// decides whether the entitlement unlocks content keys (Rails A/B) or nest
/// membership (Rail C). That is what keeps the waist simultaneously
/// **mechanism-blind and target-blind**: a webhook, a claim code, and a zap
/// receipt all reduce to this, and a payee who adds a mechanism automatically
/// monetizes every surface they gate with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentEntitlement {
    /// Mechanism that produced the entitlement — a [`PaymentProvider::kind`]
    /// for webhook ingress, `"manual"` for an author-minted claim code.
    /// Carried for audit + claim idempotency scoping, never branched on by the
    /// engine.
    pub provider: String,
    /// The user being paid. Every mechanism must resolve this itself (webhook:
    /// the URL path segment; claim redemption: the claim row's author) — the
    /// engine never infers it.
    pub payee: ActorId,
    pub buyer: Buyer,
    /// Tier name the payment maps to (via [`TierMapping`]).
    pub tier: String,
    /// Seconds since epoch; `None` = no expiry until refunded.
    pub valid_until_secs: Option<u64>,
    /// Provider-side event/payment id (idempotency + audit).
    pub external_ref: String,
    /// The provider's post-checkout reference, carried from
    /// [`VerifiedPaymentEvent::return_ref`] onto a minted claim (the visitor
    /// return door's lookup key). `None` for every non-checkout mechanism.
    pub return_ref: Option<String>,
    /// Carried from [`VerifiedPaymentEvent::purchase_refs`]: the stable
    /// per-purchase references a renewal extends by and a refund voids by.
    /// Empty for every mechanism that names no purchase (claim redemption, a
    /// zap receipt) — the engine then keys on `external_ref` alone.
    pub purchase_refs: Vec<String>,
    /// Carried from [`VerifiedPaymentEvent::product_reference`]: the product
    /// the payment named, recorded on its purchase so a later renewal (which
    /// names none) is covered only while the mapping still names this product
    /// ([`TierMapping::covers`]). `None` for every event and mechanism that
    /// names no product.
    pub product_reference: Option<String>,
}

impl PaymentEntitlement {
    /// Reduce an adapter's verified event to the waist, resolving the tier
    /// through the payee's configured `mapping`.
    ///
    /// `None` means "no mapping covers this payment" — the caller surfaces it
    /// as a conflict rather than guessing a tier.
    ///
    /// The buyer binding is normalized here, once, for every mechanism: a
    /// `buyer_reference` that hex-decodes to an actor binds
    /// ([`Buyer::Actor`]); **anything absent or unparseable falls back to
    /// [`Buyer::Unbound`]** — the buyer really did pay, only the binding
    /// failed, so the engine mints a claim code instead of dropping the
    /// payment (monetization.md § Pillar 3 Q4).
    ///
    /// Note this deliberately does NOT consider [`PaymentEventKind`]: the kind
    /// is the *verb* (grant vs void vs ignore) the engine dispatches on, while
    /// this value is the *noun* both verbs act upon. A refund names the very
    /// same entitlement it voids.
    pub fn from_verified_event(
        provider: &str,
        payee: ActorId,
        event: &VerifiedPaymentEvent,
        mapping: &TierMapping,
    ) -> Option<Self> {
        let tier = mapping.tier_for(event.product_reference.as_deref())?;
        let buyer = event
            .buyer_reference
            .as_deref()
            .and_then(|r| ActorId::from_hex(r).ok())
            .map_or(Buyer::Unbound, Buyer::Actor);
        Some(Self {
            provider: provider.to_string(),
            payee,
            buyer,
            tier: tier.to_string(),
            valid_until_secs: event.valid_until_secs,
            external_ref: event.external_ref.clone(),
            return_ref: event.return_ref.clone(),
            purchase_refs: event.purchase_refs.clone(),
            product_reference: event.product_reference.clone(),
        })
    }
}

// ── Shared verification helper ─────────────────────────────────

/// The shared `fauna-core` implementation (constant-time; `false` for
/// differing lengths — lengths aren't secret here, both sides are
/// fixed-width hex MACs on the legitimate path).
pub(crate) use fauna_core::secret::constant_time_eq;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_url_pairs_with_the_nest_ingress_route() {
        // The exact URL the creator registers at their provider's dashboard.
        // Pinned against the nest's registered axum route so the two can
        // never drift apart silently (bins/fauna-nest/src/lib.rs builds its
        // route from WEBHOOK_PATH_PREFIX).
        assert_eq!(
            webhook_url("https://nest.example", "deadbeef", "stripe"),
            "https://nest.example/api/v1/payments/webhook/deadbeef/stripe",
        );
    }

    #[test]
    fn webhook_url_tolerates_a_trailing_slash_on_the_base() {
        // A client's stored nest URL may or may not carry a trailing slash;
        // both must produce the same registerable URL (a double slash would
        // not match the nest's route).
        assert_eq!(
            webhook_url("https://nest.example/", "deadbeef", "fake"),
            webhook_url("https://nest.example", "deadbeef", "fake"),
        );
    }

    #[test]
    fn webhook_url_covers_every_known_kind() {
        for kind in known_kinds() {
            let url = webhook_url("https://n", "aa", kind);
            assert!(url.ends_with(kind), "{url} should end with {kind}");
        }
    }

    #[test]
    fn return_url_pairs_with_the_nest_return_route() {
        // The redirect a creator pastes into Stripe's checkout settings: the
        // nest registers its route from RETURN_PATH_PREFIX, and Stripe
        // substitutes the session id its webhook later reports as return_ref.
        assert_eq!(
            return_url("https://site.example/", "stripe"),
            "https://site.example/api/v1/paywall/return/stripe?ref={CHECKOUT_SESSION_ID}",
        );
        assert_eq!(
            return_url("https://site.example", "fake"),
            "https://site.example/api/v1/paywall/return/fake?ref={REF}",
        );
        assert!(REDEEM_PATH.starts_with("/api/v1/paywall/"));
    }

    #[test]
    fn headers_are_case_insensitive() {
        let h = WebhookHeaders::from_pairs([("X-Fauna-Signature", "abc")]);
        assert_eq!(h.get("x-fauna-signature"), Some("abc"));
        assert_eq!(h.get("X-FAUNA-SIGNATURE"), Some("abc"));
        assert_eq!(h.get("stripe-signature"), None);
    }

    #[test]
    fn registry_resolves_known_kinds_only() {
        for kind in known_kinds() {
            let p = provider_for_kind(kind).expect("registered kind resolves");
            assert_eq!(p.kind(), *kind);
        }
        assert!(provider_for_kind("paypal").is_none());
        assert!(provider_for_kind("").is_none());
    }

    /// A production build carries no `fake` provider: run under
    /// `cargo test --release -p fauna-payments` (no `debug_assertions`, no
    /// `test-helpers`), this is the registry a production nest and app
    /// actually link.
    #[cfg(not(any(debug_assertions, feature = "test-helpers")))]
    #[test]
    fn a_production_build_registers_no_fake_provider() {
        assert_eq!(known_kinds(), &["stripe"]);
        assert!(provider_for_kind("fake").is_none());
    }

    /// The other half: a debug or e2e build keeps the fake provider every
    /// tier_3 suite drives.
    #[cfg(any(debug_assertions, feature = "test-helpers"))]
    #[test]
    fn a_debug_or_e2e_build_registers_the_fake_provider() {
        assert!(known_kinds().contains(&"fake"));
        assert!(provider_for_kind("fake").is_some());
    }

    #[test]
    fn single_tier_mapping_maps_everything() {
        let m = TierMapping {
            tier: "gold".into(),
            product: None,
        };
        assert_eq!(m.tier_for(None), Some("gold"));
        assert_eq!(m.tier_for(Some("prod_123")), Some("gold"));
    }

    // ── The narrow waist ───────────────────────────────────────

    fn payee() -> ActorId {
        ActorId([0xAA; 32])
    }

    fn gold() -> TierMapping {
        TierMapping {
            tier: "gold".into(),
            product: None,
        }
    }

    #[test]
    fn a_mapping_with_no_product_covers_every_payment() {
        let ev = payment_event(None);
        assert!(gold().covers(&ev, None));
        let mut bare = ev.clone();
        bare.product_reference = None;
        assert!(gold().covers(&bare, None));
    }

    #[test]
    fn a_product_mapping_covers_only_its_product_or_a_recorded_purchase() {
        let m = TierMapping {
            tier: "gold".into(),
            product: Some("plink_gold".into()),
        };
        let mut ev = payment_event(None);
        ev.product_reference = Some("plink_gold".into());
        assert!(m.covers(&ev, None), "the mapped product");
        ev.product_reference = Some("plink_tips".into());
        assert!(!m.covers(&ev, None), "another product on the account");
        ev.product_reference = None;
        assert!(!m.covers(&ev, None), "a payment naming no product");
        // A renewal names no product, only its purchase — covered once the
        // purchase was recorded under the mapped product, and only then.
        assert!(
            m.covers(&ev, Some("plink_gold")),
            "a renewal of a purchase of the mapped product"
        );
        assert!(
            !m.covers(&ev, Some("plink_old")),
            "a renewal of a purchase sold as another product"
        );
    }

    #[test]
    fn a_product_mapping_never_withholds_a_refund_or_an_ignored_event() {
        let m = TierMapping {
            tier: "gold".into(),
            product: Some("plink_gold".into()),
        };
        let mut ev = payment_event(None);
        ev.product_reference = None;
        for kind in [PaymentEventKind::Refund, PaymentEventKind::Ignored] {
            ev.kind = kind;
            assert!(m.covers(&ev, None), "{kind:?}");
        }
    }

    fn payment_event(buyer_reference: Option<&str>) -> VerifiedPaymentEvent {
        VerifiedPaymentEvent {
            kind: PaymentEventKind::Payment,
            buyer_reference: buyer_reference.map(str::to_string),
            product_reference: Some("prod_123".into()),
            valid_until_secs: Some(1_800_000_000),
            external_ref: "evt_1".into(),
            return_ref: Some("cs_test_1".into()),
            purchase_refs: vec!["sub_1".into()],
        }
    }

    #[test]
    fn waist_binds_a_parseable_buyer_reference_to_an_actor() {
        // client_reference_id = actor_id hex (Q4's primary binding).
        let buyer_hex = "11".repeat(32);
        let e = PaymentEntitlement::from_verified_event(
            "stripe",
            payee(),
            &payment_event(Some(&buyer_hex)),
            &gold(),
        )
        .expect("the single mapping covers every payment");

        assert_eq!(e.buyer, Buyer::Actor(ActorId([0x11; 32])));
        // Every other field is carried through verbatim — the waist normalizes
        // the binding and the tier, and invents nothing else.
        assert_eq!(e.provider, "stripe");
        assert_eq!(e.payee, payee());
        assert_eq!(e.tier, "gold");
        assert_eq!(e.valid_until_secs, Some(1_800_000_000));
        assert_eq!(e.external_ref, "evt_1");
        assert_eq!(e.return_ref.as_deref(), Some("cs_test_1"));
        assert_eq!(e.purchase_refs, vec!["sub_1".to_string()]);
        assert_eq!(e.product_reference.as_deref(), Some("prod_123"));
    }

    #[test]
    fn waist_leaves_an_absent_buyer_reference_unbound() {
        // No binding carried through checkout → the engine mints a claim code;
        // the payment is never dropped.
        let e = PaymentEntitlement::from_verified_event(
            "stripe",
            payee(),
            &payment_event(None),
            &gold(),
        )
        .expect("mapping covers it");
        assert_eq!(e.buyer, Buyer::Unbound);
    }

    #[test]
    fn waist_falls_back_to_unbound_on_an_unparseable_buyer_reference() {
        // The buyer DID pay — only the binding failed. Falling back to the
        // claim-code path (rather than erroring) is the ratified behavior;
        // treat every malformed shape the same way.
        for bad in ["not-hex", "", "1234", &"zz".repeat(32), &"11".repeat(31)] {
            let e = PaymentEntitlement::from_verified_event(
                "stripe",
                payee(),
                &payment_event(Some(bad)),
                &gold(),
            )
            .expect("mapping covers it");
            assert_eq!(e.buyer, Buyer::Unbound, "buyer_reference {bad:?}");
        }
    }

    #[test]
    fn waist_takes_its_tier_from_the_mapping_and_never_invents_one() {
        // The `None` return ("no mapping covers this payment") is genuinely
        // unreachable with the first-cut single mapping, whose `tier_for` is
        // total — so this pins the reachable half of the contract instead of
        // faking the unreachable one: the resolved tier is EXACTLY what
        // `tier_for` returned for this product, never the mapping's field read
        // directly and never a default. A per-product map slots in additively
        // behind that call, and the `?` keeps the None arm wired for it.
        let m = TierMapping {
            tier: "platinum".into(),
            product: None,
        };
        let ev = payment_event(None);
        let e = PaymentEntitlement::from_verified_event("fake", payee(), &ev, &m)
            .expect("the first-cut mapping resolves every product");
        assert_eq!(
            Some(e.tier.as_str()),
            m.tier_for(ev.product_reference.as_deref())
        );
        assert_eq!(e.tier, "platinum");
    }

    #[test]
    fn waist_is_the_same_value_whatever_the_event_kind() {
        // The kind is the VERB (grant/void/ignore) the engine dispatches on;
        // the entitlement is the NOUN both verbs act upon. A refund names the
        // very same entitlement it voids — so normalization must not vary.
        let buyer_hex = "11".repeat(32);
        let mut payment = payment_event(Some(&buyer_hex));
        let grant = PaymentEntitlement::from_verified_event("fake", payee(), &payment, &gold())
            .expect("mapping covers it");

        payment.kind = PaymentEventKind::Refund;
        let refund = PaymentEntitlement::from_verified_event("fake", payee(), &payment, &gold())
            .expect("mapping covers it");

        assert_eq!(grant, refund);
    }
}
