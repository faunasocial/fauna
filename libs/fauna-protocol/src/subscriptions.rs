//! WS-RPC request/reply types for the `fauna.subscriptions.*` namespace.
//!
//! Migrates the 15 authenticated subscription-management routes from
//! `bins/fauna-nest/src/subscription_routes.rs` to WS-RPC kinds per
//! `docs/goal/architecture/transport.md` and the migration plan in
//! `docs/goal/architecture/api-layers.md` § WS-RPC migration status.
//!
//! Design tracked internally.

use crate::Value;
use fauna_core::data::Timestamp;
use fauna_core::encoding::EmbedAsBytes;
use fauna_core::identity::ActorId;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

// ── the asking price ───────────────────────────────────────────

/// A tier's **machine-comparable purchase threshold** on the wire
/// (`monetization.md` § The asking price, ratified 2026-08-02 / Q10).
///
/// The wire twin of [`fauna_payments::asking_price::AskingPrice`], which owns
/// the comparison rule. Same split as `Tip` / [`crate::tips::TipItem`]: the
/// concept and its logic live in the wasm32-clean `fauna-payments` crate, the
/// serde-bearing wire shape lives here, and neither crate depends on the
/// other. The nest converts at the one comparison site.
///
/// **Unit-tagged, never a bare `price_msats`.** A bare millisatoshi field is
/// the Lightning special-case § *One model, many mechanisms, two targets*
/// forbids; `unit` is a **denomination** tag, never a mechanism tag, so a zap,
/// an eCash note and a Lightning-invoice webhook denominated alike compare
/// alike. A future fiat mechanism adds a unit arm additively.
///
/// **Carried, never interpreted, when the unit is unknown.** A newer client
/// may price a tier in a unit this build has never heard of; the field
/// round-trips unchanged and the comparison answers *not met*, fail-closed
/// (`version-compatibility.md` — additive everywhere, bidirectional within a
/// major).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TierAskingPrice {
    /// Amount asked, in `unit`'s denomination. `0` is legal and means "any
    /// amount in this unit buys it" — distinct from carrying no asking price
    /// at all, which means no inferring mechanism can buy the tier.
    pub value: u64,
    /// Denomination tag — `"msat"` ([`UNIT_MSAT`]) is the first and, today,
    /// only unit any mechanism reports.
    pub unit: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Millisatoshis — the first ratified denomination, and the one every current
/// amount-bearing mechanism (today: NIP-57 zap receipts) reports in.
///
/// **A wire token, so it lives at the wire layer.** A shipped nest and a
/// shipped client compare these strings, which is why this constant may never
/// be renamed to follow a Rust identifier. It sits here rather than in
/// `fauna-payments` because [`TierAskingPrice`] must keep round-tripping a
/// priced tier in builds where the payments plane is compiled away
/// (`dynamic-features.md` § Compile-time excision — an excised build is a peer
/// without a capability, never a fork of the wire).
/// `fauna_payments::asking_price` re-exports it, so the comparison rule and
/// the wire shape can never drift onto two different tokens.
pub const UNIT_MSAT: &str = "msat";

/// Millisatoshis per satoshi — re-exported under the name the payments plane
/// publishes (`fauna_payments::asking_price` re-exports it in turn). The factor
/// itself is owned by [`fauna_core::money::MSATS_PER_SAT`]: this crate sits
/// *above* `fauna-core`, so a declaration here could never be read by
/// `fauna-core`'s own consumers — which is exactly how this constant came to
/// have four declarations while claiming to be the one place it was written.
pub use fauna_core::money::MSATS_PER_SAT;

impl TierAskingPrice {
    /// Build a msat-denominated asking price from the author's own unit, sats.
    ///
    /// **The one home of this arithmetic.** Every surface that lets an author
    /// name a price (the FFI tier editor, the wasm one, the feed's sell-gated
    /// compose) converts here, so no app writes the multiply itself — the
    /// discipline `monetization.md` § The asking price asks for, and the reason
    /// this is a constructor on the wire type rather than a free function in
    /// the payments crate: it stays reachable when that crate is excised.
    ///
    /// `None` iff the amount is too large to express in msats — refused rather
    /// than clamped, since a silently-clamped price is a sale at the wrong
    /// number.
    pub fn from_sats(sats: u64) -> Option<Self> {
        Some(Self {
            value: sats.checked_mul(MSATS_PER_SAT)?,
            unit: UNIT_MSAT.to_string(),
            extra: BTreeMap::new(),
        })
    }

    /// The inverse of [`from_sats`](Self::from_sats) — the tier's price in
    /// sats, for an edit form to pre-fill from `TierItem::asking_price`. The
    /// same "one home of this arithmetic" reasoning applies to the reverse
    /// direction: no app hand-derives `value / MSATS_PER_SAT`.
    ///
    /// `None` for a unit this build does not recognize (fail-closed, the same
    /// posture `is_met_by` takes on a mismatch) or a value that is not a whole
    /// number of sats — `from_sats` never produces one, but a stored value
    /// this build cannot fully interpret must not silently truncate into a
    /// DIFFERENT price the author never set.
    pub fn to_sats(&self) -> Option<u64> {
        if self.unit != UNIT_MSAT {
            return None;
        }
        self.value
            .is_multiple_of(MSATS_PER_SAT)
            .then_some(self.value / MSATS_PER_SAT)
    }

    /// A msat-denominated price directly, for callers that already hold msats
    /// (the nest's storage path and its tests).
    ///
    /// Here for the same reason [`from_sats`](Self::from_sats) is: it must stay
    /// reachable where the payments crate is excised. It is the wire twin of
    /// `fauna_payments::asking_price::AskingPrice::msats`, which remains the
    /// constructor for the *comparison* type — the one thing an excised build
    /// genuinely does not have, because judging that money met a price is the
    /// act the `payments` member gates.
    pub fn msats(value: u64) -> Self {
        Self {
            value,
            unit: UNIT_MSAT.to_string(),
            extra: BTreeMap::new(),
        }
    }
}

// ── tiers.create ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TierCreateRequest {
    pub name: String,
    pub rank: u32,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub price_hint: Option<String>,
    #[serde(default)]
    pub payment_url: Option<String>,
    pub auto_approve: bool,
    /// The tier's **birth KeyBlob** — the empty-roster blob minted by the
    /// author's client under the fresh period key, so every client-minted tier
    /// has a live `KeyBlob` from creation (`ui/feed.md` § Encryption at rest —
    /// broadcast tiers lists `tiers.create` among the upload-carrying kinds).
    /// This is what lets a creator gate a post to a tier *before* its first
    /// subscriber (the compose leg reads the blob's hash as
    /// `GatedInfo.key_blob_ref`). **Required**: the envelope-less create an
    /// older client once sent was removed by the compat-remnant sweep
    /// (`version-compatibility.md` § Dimension 2, the fourth ratified
    /// exception), so a request without it fails to decode.
    pub encrypted_upload: EncryptedKeyBlobUpload,
    /// **Per-post pay-to-unlock designation** — the hex `post_id`
    /// (`blake3(body)`, 32 bytes) this tier sells access to, making it a
    /// *degenerate single-post tier* (`monetization.md` § Per-post
    /// pay-to-unlock, Q9). `None` on every ordinary tier.
    ///
    /// **Create-time immutable**: `tiers.update` refuses to change it, because
    /// re-pointing a sold unlock is a rug-pull. The nest deliberately does
    /// **not** check that the post exists — the author's client mints the
    /// period key, builds the birth `KeyBlob`, builds the gated post body
    /// (which carries this tier's name) and only then knows
    /// `post_id = blake3(body)`, so the post is created *after* this call.
    /// Format is validated, existence is not; the tier row also outlives the
    /// post by design (`monetization.md:131`).
    ///
    /// Additive + `None`-default (`version-compatibility.md` I4): a create
    /// without it lands undesignated.
    #[serde(default)]
    pub unlocks_post: Option<String>,
    /// **The machine-comparable asking price** (`monetization.md` § The asking
    /// price). `None` on every tier that is not for sale to an *inferring*
    /// mechanism — which is the out-of-the-box default and stays permanently
    /// correct: inferred sale is opt-in, and a tier carrying only the
    /// human-readable `price_hint` deliberately cannot be bought by zap (no
    /// free-text parsing ever infers a number).
    ///
    /// Unlike [`Self::unlocks_post`] this is **mutable** — see
    /// [`TierUpdateRequest::asking_price`]. A price edit binds *future* events
    /// only; entitlements already granted persist unchanged, so no rug-pull
    /// semantics arise.
    ///
    /// Additive + `None`-default (`version-compatibility.md` I4): a create
    /// without it lands unpriced.
    #[serde(default)]
    pub asking_price: Option<TierAskingPrice>,
    /// **Hidden from every offer surface.** A hidden tier is carried on the
    /// author's own `tiers.list` read and nowhere else: `offers.list`, the
    /// unauthenticated HTTP tier read and the compose gate picker all omit it
    /// (`monetization.md` § The unifying model — *A tier may be hidden*). Its
    /// one use today is the reserved owner-only tier the archive import mints
    /// (`fauna_core::subscription::OWNER_ONLY_TIER`). Create-time immutable
    /// like [`Self::unlocks_post`] — no counterpart on `TierUpdateRequest`.
    ///
    /// Additive + `false`-default (`version-compatibility.md` I4): a create
    /// without it lands offered.
    #[serde(default)]
    pub hidden: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TierCreateReply {
    pub created: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── tiers.update ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TierUpdateRequest {
    pub name: String,
    #[serde(default)]
    pub rank: Option<u32>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub price_hint: Option<String>,
    #[serde(default)]
    pub payment_url: Option<String>,
    #[serde(default)]
    pub auto_approve: Option<bool>,
    /// The per-post pay-to-unlock designation — present here **only so the
    /// refusal is loud**. It is create-time immutable
    /// (`monetization.md:131`), so this handler accepts `None` (the ordinary
    /// "keep current" merge) and a value equal to the current one (an
    /// auto-retrying client must not be turned into an error), and refuses
    /// anything else with `fauna.subscriptions.designation_immutable` —
    /// including attaching a designation to a tier created without one.
    ///
    /// Omitting the field from this type would make the designation
    /// structurally unchangeable but route a client's attempt into `extra`
    /// and answer `updated: true` — a silent drop that reads as success.
    #[serde(default)]
    pub unlocks_post: Option<String>,
    /// The machine-comparable asking price — **mutable here**, deliberately
    /// unlike [`Self::unlocks_post`] beside it (`monetization.md` § The asking
    /// price — *Editability*). Re-pointing a sold unlock is a rug-pull;
    /// re-pricing is not, because a price edit binds **future** events only
    /// and every entitlement already granted persists unchanged.
    ///
    /// `None` keeps the current value, exactly like every other optional field
    /// on this handler. **There is therefore no way to CLEAR a price back to
    /// unset** — a `None`-means-clear reading would make an older client's
    /// omission silently wipe the field, and the tri-state that would express
    /// both (`Option<Option<T>>`) does not round-trip on DAG-CBOR (`None` and
    /// `Some(None)` both encode as `null`). That gap is **pre-existing and
    /// shared** with `description` / `price_hint` / `payment_url` here, and is
    /// captured as one uniform track rather than solved bespokely for this one
    /// field. Until then an
    /// author who wants to stop inferred sales sets a price no zap will reach.
    #[serde(default)]
    pub asking_price: Option<TierAskingPrice>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TierUpdateReply {
    pub updated: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── tiers.clear_field ─────────────────────────────────────────

/// The [`TierUpdateRequest`] optional field a [`TierClearFieldRequest`] wipes
/// back to unset. `tiers.update`'s `None` means "keep the current value" for
/// every one of these, so it has no way to express "clear this" — the
/// `Option<Option<T>>` tri-state that would say so does not round-trip on
/// DAG-CBOR (`None` and `Some(None)` both encode `null`, `subscriptions.rs`
/// documents the same gap on each field this covers). This is the
/// thrice-ratified dedicated-setter precedent (`SetRoleAddressRequest` /
/// `SetDkimRotationDaysRequest` / `SetCatchAllActorRequest`,
/// `bridge_routing.rs`) applied here — but unlike those, clearing carries no
/// value to set (`tiers.update` already sets), so one field-selector enum
/// covers all four fields instead of one dedicated kind per field: there is
/// no value-type heterogeneity to hide behind a shared shape.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TierClearableField {
    #[default]
    Description,
    PriceHint,
    PaymentUrl,
    AskingPrice,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TierClearFieldRequest {
    pub name: String,
    pub field: TierClearableField,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TierClearFieldReply {
    pub cleared: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── tiers.delete ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TierDeleteRequest {
    pub name: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TierDeleteReply {
    pub deleted: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── tiers.list ─────────────────────────────────────────────────

/// Authenticated read of the *calling* author's own tier definitions. Zero
/// args — the nest keys on the bearer, never a request-supplied id (so it
/// cannot be used to read another actor's tiers; the public per-author HTTP
/// read serves that case). Mirrors the empty `RequestsListRequest`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TiersListRequest {}

/// One subscription tier as the author sees it. Superset of the public HTTP
/// `list_tiers` shape — it additionally carries `auto_approve` (the HTTP twin
/// omits it) so the edit form round-trips it.
///
/// `Default` exists for the **fixture-conflict** reason that applies to any
/// growing wire type: two branches that each add a field to this struct merge
/// cleanly, because fixtures say `..Default::default()` instead of
/// hand-listing every field and colliding on the grown axis.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TierItem {
    pub name: String,
    pub rank: u32,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub price_hint: Option<String>,
    #[serde(default)]
    pub payment_url: Option<String>,
    pub auto_approve: bool,
    pub created_at: Timestamp,
    /// The per-post pay-to-unlock designation (hex `post_id`), `None` on
    /// every ordinary tier — see [`TierCreateRequest::unlocks_post`].
    ///
    /// Carried on the **author's own** `tiers.list` read (they need their
    /// unlock tiers to gate the post and to audit sales), and filtered out
    /// entirely from the generic offer surfaces — `offers.list` and the
    /// public HTTP tier read — since the unlock affordance renders on the
    /// post, not in a tier browse (`monetization.md:128`). The §1 My-tiers
    /// management list and the compose gate picker exclude designated tiers
    /// client-side off this field.
    ///
    /// Additive + `None`-default (`version-compatibility.md` I4).
    #[serde(default)]
    pub unlocks_post: Option<String>,
    /// The machine-comparable asking price, `None` when the tier is not for
    /// sale to an inferring mechanism — see
    /// [`TierCreateRequest::asking_price`].
    ///
    /// Carried on the **author's own** read so the tier edit form round-trips
    /// it. It is deliberately absent from the buyer-facing offer surfaces,
    /// which keep rendering `price_hint`: that string stays the human-readable
    /// price and the zero-integration floor, while this number exists for
    /// machines to compare. Two independent fields on purpose — an author may
    /// set either, both, or neither, and fauna still parses, charges, and
    /// moves nothing.
    ///
    /// Additive + `None`-default (`version-compatibility.md` I4).
    #[serde(default)]
    pub asking_price: Option<TierAskingPrice>,
    /// Hidden from every offer surface — see [`TierCreateRequest::hidden`].
    /// Only ever `true` on the author's own `tiers.list` read; the offer
    /// surfaces filter hidden tiers out nest-side before this is built.
    ///
    /// Additive + `false`-default (`version-compatibility.md` I4).
    #[serde(default)]
    pub hidden: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Hand-written because [`Timestamp`] has no `Default` — deriving would
/// require giving a shared core type an epoch-zero default for one fixture's
/// convenience. The value is what a `Default` derive would produce anyway.
impl Default for TierItem {
    fn default() -> Self {
        Self {
            name: String::new(),
            rank: 0,
            description: None,
            price_hint: None,
            payment_url: None,
            auto_approve: false,
            created_at: Timestamp(0),
            unlocks_post: None,
            asking_price: None,
            hidden: false,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TiersListReply {
    /// The author's tiers, ascending by `rank`.
    pub tiers: Vec<TierItem>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── offers.list ────────────────────────────────────────────────

/// Authenticated read of **another** actor's offered tiers — the
/// subscriber-browse source for the profile Tiers tab when viewing someone
/// else's profile (`docs/goal/ui/profile.md` § Layout & flow → *Another's
/// profile (subscriber browse)*; `docs/goal/behavior/monetization.md`
/// § Pillar 1 — `subscription-offers-section`). Carries the target
/// `author_id` (unlike the bearer-keyed own-read [`TiersListRequest`]).
///
/// This is the WS-RPC successor to the public per-author HTTP read
/// `GET /api/v1/subscriptions/tiers/{author_id}` for the **authenticated
/// in-client** browse: every Fauna app already holds an authenticated
/// WS-RPC connection, so it reads offers over the wire uniformly with
/// `status.get` / `subscribe`, with no per-app HTTP glue (priority #2).
/// The HTTP read remains for genuinely *unauthenticated* external / web-paywall
/// consumers. Tier definitions are public, so any authenticated caller may read
/// any author's — exactly the data the HTTP route already serves to anyone.
/// Reply is [`TiersListReply`] (a list of tier offerings, ascending by rank).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OffersListRequest {
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub author_id: ActorId,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── post_unlock.get ────────────────────────────────────────────

/// Authenticated point read of one sold post's **public purchase fields** —
/// the self-serve teaser's price read (`monetization.md` § Per-post
/// pay-to-unlock → *the buyer's price read is post-addressed*, ruled
/// 2026-07-29). Keyed `(author_id, post_id)` and **post-addressed, never
/// name-addressed**: `post_id = blake3(body)` is unguessable without having
/// seen the post, so possession of the id is evidence of legitimate teaser
/// access and the offer-surface filters' anti-enumeration property survives —
/// a by-name read would re-open probing the author's regular tier namespace.
/// Answers iff one of the author's tiers carries `unlocks_post == post_id`;
/// an undesignated post, a foreign id, and an unknown author are all the same
/// empty reply. USER-class, ordinary dispatch limits (an indexed point read —
/// no bespoke throttle). On any error (transport, undesignated id) the client
/// renders the teaser without a price and claim-code redemption remains the
/// fallback purchase path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostUnlockGetRequest {
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub author_id: ActorId,
    /// Hex 32-byte post id (the [`crate::posts::PostCreateReply::post_id`]
    /// shape). Malformed ⇒ refused, same as `tiers.create`'s designation.
    pub post_id: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PostUnlockGetReply {
    /// `Some` iff one of the author's tiers designates the asked post;
    /// `None` says only "nothing is for sale under that id here" — which
    /// possession of a designated id already distinguishes.
    #[serde(default)]
    pub offer: Option<PostUnlockOffer>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The public purchase fields of one designated unlock tier — deliberately a
/// subset of [`TierItem`] (no rank, no `auto_approve`, no timestamps): exactly
/// what the teaser affordance renders, so this read never grows into a fourth
/// generic tier surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostUnlockOffer {
    /// The unlock tier's name (`post-unlock-<16 hex>`) — what the buyer's
    /// subscribe / claim call names.
    pub tier_name: String,
    #[serde(default)]
    pub price_hint: Option<String>,
    #[serde(default)]
    pub payment_url: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── subscribe / unsubscribe ────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubscribeRequest {
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub author_id: ActorId,
    pub tier: String,
    /// The subscriber's 1184-byte ML-KEM-768 encapsulation key (post-quantum
    /// surface B, slice S4). Published here — the only subscriber-originated
    /// message — so the author can wrap the period key to the subscriber's
    /// X-Wing hybrid key. `None` (a classical-only subscriber) ⇒ the author
    /// seals classical. Additive: omitted on the wire when
    /// `None`, and the wrap side validates length + degrades to classical on any mismatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mlkem_encaps_key: Option<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SubscribeReply {
    Approved {
        tier: String,
        #[serde(default)]
        expires_at: Option<Timestamp>,
    },
    Queued {
        request_id: i64,
    },
    /// An outcome a newer build added that this build cannot read
    /// (`transport.md` § Schema and forward-compat discipline, rule 3: open,
    /// collapsing). The request was sent and its state is unknown: the caller re-reads the subscription status. Never serialized: a path that would re-emit it
    /// fails instead of replacing the newer value.
    #[serde(other, skip_serializing)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnsubscribeRequest {
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub author_id: ActorId,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum UnsubscribeReply {
    Removed,
    Queued {
        request_id: i64,
    },
    /// An outcome a newer build added that this build cannot read
    /// (`transport.md` § Schema and forward-compat discipline, rule 3: open,
    /// collapsing). As [`SubscribeReply::Unknown`]: the caller re-reads the subscription status. Never serialized: a path that would re-emit it
    /// fails instead of replacing the newer value.
    #[serde(other, skip_serializing)]
    Unknown,
}

// ── status.get ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusGetRequest {
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub author_id: ActorId,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusGetReply {
    pub tier: Option<String>,
    pub expires_at: Option<Timestamp>,
    pub auto_approve: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── mine.list ──────────────────────────────────────────────────

/// Authenticated *consumer-side* read of the **calling actor's own
/// subscriptions across every creator**. Zero args — the nest keys on the
/// bearer, never a request-supplied id (so it cannot enumerate another actor's
/// subscriptions). Powers the `subscription-settings` page `subscription-mine-list`
/// (`monetization.md` § Pillar 1). Distinct from `status.get`, which is keyed on
/// a request-supplied `author_id` (the per-creator status on a profile).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MineListRequest {}

/// One of the caller's subscriptions, per `(creator, tier)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MineSubscription {
    /// The creator the caller subscribes to.
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub author_id: ActorId,
    /// The subscribed tier name.
    pub tier: String,
    /// `"active"` (an approved subscriber row) or `"pending"` (a queued
    /// subscribe request not yet approved — the encrypted-mode default). The
    /// raw wire enum string, rendered verbatim by the consumer page (uniform
    /// with `PendingRequest.kind`). Surfaces `subscription-mine-status`.
    pub status: String,
    /// The creator's handle if the nest can resolve it locally (the creator is
    /// a local account), else `None` — the client falls back to the hex actor
    /// id. Best-effort; never a hard dependency.
    #[serde(default)]
    pub handle: Option<String>,
    /// When the subscription reached its current state: `approved_at` for
    /// `active`, `created_at` for `pending`. Epoch micros (the wire convention).
    pub since: Timestamp,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MineListReply {
    /// The caller's subscriptions across all creators; active rows first.
    pub subscriptions: Vec<MineSubscription>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── requests.list / approve / reject ───────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestsListRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingRequest {
    pub request_id: i64,
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub subscriber_id: ActorId,
    pub tier_name: String,
    /// "subscribe" | "unsubscribe"
    pub kind: String,
    pub created_at: Timestamp,
    /// The pending subscriber's 1184-byte ML-KEM-768 encapsulation key
    /// (post-quantum surface B, slice S4b), surfaced on the request so an
    /// encrypted-mode author's client can wrap the period key to the *brand-new*
    /// subscriber's X-Wing hybrid key **before** they land on the roster (a
    /// `subscribers.list` entry only exists post-approval). `None` = the
    /// subscriber published no post-quantum key ⇒ the author seals classical.
    /// Additive + forward-compat (omitted on the wire when `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mlkem_encaps_key: Option<ByteBuf>,
    /// Verified-payment marker (monetization.md § Pillar 3): a payment
    /// provider verified this subscriber paid for this tier, so the author's
    /// drain pump approves the request without creator judgment — the third
    /// grant source next to manual approval and tier `auto_approve` (whose
    /// discriminant stays tier-side on `tiers.list`; this one is per-request
    /// because a paid tier is not auto-approve). Additive: it defaults `false` for a non-paid request (drain skips,
    /// author approves by hand — fail-safe).
    #[serde(default)]
    pub payment_entitled: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestsListReply {
    pub requests: Vec<PendingRequest>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EncryptedKeyBlobUpload {
    /// Signed KeyBlob in the embed-as-bytes wire shape
    /// (`docs/goal/architecture/transport.md` § Embed-as-bytes for signed
    /// payloads). The bytes are the canonical dag-cbor encoding of the
    /// `KeyBlob` value; the envelope binds them to `signer_auth.device_key`.
    pub key_blob: EmbedAsBytes,
    /// Signed DeviceAuthorization in the embed-as-bytes wire shape (signed
    /// by `key_blob.author`, carries `ManageSubscribers` capability).
    pub signer_auth: EmbedAsBytes,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApproveRequestRequest {
    pub request_id: i64,
    #[serde(default)]
    pub encrypted_upload: Option<EncryptedKeyBlobUpload>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApproveRequestReply {
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub subscriber: ActorId,
    pub tier: String,
    pub key_version: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RejectRequestRequest {
    pub request_id: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RejectRequestReply {
    pub rejected: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── key_blob.get ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeyBlobGetRequest {
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub author_id: ActorId,
    pub tier_name: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeyBlobGetReply {
    pub version: u64,
    pub blob_hash: ByteBuf,
    /// dag-cbor-encoded KeyBlob.
    pub blob_data: ByteBuf,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── subscribers.list / remove ──────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubscribersListRequest {
    pub tier_name: String,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubscriberEntry {
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub subscriber_id: ActorId,
    pub joined_at: Timestamp,
    /// The subscriber's 1184-byte ML-KEM-768 encapsulation key (post-quantum
    /// surface B, slice S4), surfaced on the roster so an encrypted-mode author's
    /// client wraps the period key to the subscriber's X-Wing hybrid key. `None`
    /// = the subscriber published no post-quantum key ⇒ the author seals
    /// classical. Additive + forward-compat (omitted on the wire when `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mlkem_encaps_key: Option<ByteBuf>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubscribersListReply {
    pub subscribers: Vec<SubscriberEntry>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoveSubscriberRequest {
    pub tier_name: String,
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub subscriber_id: ActorId,
    #[serde(default)]
    pub encrypted_upload: Option<EncryptedKeyBlobUpload>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoveSubscriberReply {
    /// Rides as a 32-byte CBOR byte string — `ActorId`'s wire shape.
    pub subscriber: ActorId,
    pub tier: String,
    pub key_version: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── key_blob.rotate ────────────────────────────────────────────

/// `fauna.subscriptions.key_blob.rotate` — republish a tier's broadcast
/// `KeyBlob` under a **fresh period key** with the roster left exactly as it
/// is.
///
/// Every other door that lands a `KeyBlob` rides a roster change
/// (`requests.approve` adds one member, `subscribers.remove` drops one), so a
/// tier whose membership is stable had no way to re-key at all. The
/// successor-side post-succession rotation is the case that needs one: a seed
/// thief read the author's period keys, so the live period key
/// is compromised while the roster is untouched (`succession-aftermath.md`
/// § Re-key scope, the tier row).
///
/// Author-scoped: the bearer must be the author (or a `ManageSubscribers`
/// delegate that signed the blob), enforced by the same
/// `verify_encrypted_upload` chain the two roster doors use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RotateKeyBlobRequest {
    pub tier_name: String,
    /// The freshly minted blob covering the **unchanged** roster. Required, as
    /// on the two roster doors: only the author's client holds the tier's key.
    pub encrypted_upload: EncryptedKeyBlobUpload,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RotateKeyBlobReply {
    pub tier: String,
    /// The stored blob's new version — the same counter
    /// `RemoveSubscriberReply::key_version` reports.
    pub key_version: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── delegate.upload ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DelegateUploadRequest {
    /// Signed DeviceAuthorization in the embed-as-bytes wire shape for the
    /// bearer's nest-key delegation.
    pub authorization: EmbedAsBytes,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DelegateUploadReply {
    pub uploaded: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Moved here from `fauna_payments::asking_price` along with the conversion
    // itself (2026-08-10): these must keep running in the excised flavor,
    // where the payments crate does not exist.

    #[test]
    fn the_author_types_sats_and_the_wire_carries_msats() {
        let price = TierAskingPrice::from_sats(21).unwrap();
        assert_eq!(price.value, 21_000);
        assert_eq!(price.unit, UNIT_MSAT);
        // Zero converts to zero, staying the legal "name your price" state
        // rather than collapsing into "no price" — which is `Option::None` at
        // the call site and never reaches this constructor.
        let zero = TierAskingPrice::from_sats(0).unwrap();
        assert_eq!(zero.value, 0);
        assert_eq!(zero.unit, UNIT_MSAT);
    }

    #[test]
    fn an_overflowing_sat_amount_converts_to_nothing_rather_than_a_clamp() {
        // A saturating clamp would price the tier at u64::MAX msats — quietly
        // unbuyable, and never shown to the author as the input error it is.
        assert_eq!(TierAskingPrice::from_sats(u64::MAX), None);
        assert_eq!(
            TierAskingPrice::from_sats(u64::MAX / MSATS_PER_SAT + 1),
            None
        );
        // The largest convertible amount still converts.
        assert!(TierAskingPrice::from_sats(u64::MAX / MSATS_PER_SAT).is_some());
    }

    #[test]
    fn a_converted_price_round_trips_the_wire_unchanged() {
        // The excision-critical property: an excised build must keep decoding
        // and re-emitting a priced tier a full client authored
        // (`dynamic-features.md` § Wire-compat posture). This test runs in BOTH
        // flavors, which is what makes it a compat pin rather than a codec
        // formality.
        let price = TierAskingPrice::from_sats(1_234).unwrap();
        let bytes = crate::encode_canonical(&price).unwrap();
        let back: TierAskingPrice = crate::decode_strict(&bytes).unwrap();
        assert_eq!(back, price);
    }

    #[test]
    fn to_sats_is_the_exact_inverse_of_from_sats() {
        for sats in [0, 1, 21, 1_234, u64::MAX / MSATS_PER_SAT] {
            let price = TierAskingPrice::from_sats(sats).unwrap();
            assert_eq!(price.to_sats(), Some(sats), "round-trip for {sats} sats");
        }
    }

    #[test]
    fn to_sats_fails_closed_on_an_unrecognized_unit_or_a_non_whole_sat_value() {
        // A unit this build cannot interpret must not silently guess a price —
        // never fall back to treating the raw value as sats.
        assert_eq!(
            TierAskingPrice {
                value: 5,
                unit: "btc".into(),
                extra: Default::default(),
            }
            .to_sats(),
            None
        );
        // A msat value that isn't a whole number of sats can never have come
        // from `from_sats` — a defensive read, not a truncation.
        assert_eq!(TierAskingPrice::msats(1_500).to_sats(), None);
        assert_eq!(TierAskingPrice::msats(2_000).to_sats(), Some(2));
    }
}
