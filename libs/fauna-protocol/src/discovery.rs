//! Pre-identity public-discovery WS-RPC payload types —
//! `fauna.nest.info`, `fauna.handle.available`, `fauna.nest.resolve`,
//! `fauna.actor.by_handle`, `fauna.setup.status`. A behavior-preserving
//! transport migration of the public GET routes `/api/v1/{node-info,
//! handle-available/{h},resolve-node/{d},actor/by-handle/{h},setup-status}`
//! (`bins/fauna-nest/src/registration.rs`; the `setup-status` HTTP twin was
//! removed in S4c2 so `fauna.setup.status` is now its sole transport). The
//! shared read logic lives in `bins/fauna-nest/src/discovery_core.rs` (the
//! remaining HTTP twins call the same fns). These kinds run on the **anonymous**
//! WS connection
//! (`GET /api/v1/ws`, no bearer) — the Discovery row of the pre-identity
//! allowlist in `docs/goal/architecture/transport.md` § Pre-identity (anonymous)
//! connection. Track A2 of the WS-RPC-everywhere migration (tracked internally).
//!
//! Wire convention (matching `auth.rs` / `posts.rs` / `conversations.rs`):
//! identity references (`actor_id`, `nest_id`) are **hex-encoded `String`**;
//! the dag-cbor wire forbids floats (none here — all `bool`/`String`/`u*`) and
//! does not round-trip `Option<Option>` (none used). The HTTP twins emit a few
//! conditional JSON keys (`cooldown`, `addresses`); the typed wire carries the
//! same information uniformly — `cooldown: bool` (default `false`) and
//! `addresses: Vec<String>` (empty when subhandles are disabled) — since no
//! client consumes the WS-RPC discovery surface yet (the conditional-key
//! quirk was an HTTP-JSON artifact).
//!
//! Kind registry: `kind.rs::register_discovery_kinds`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

/// Build/version capability tokens advertised on the anonymous `fauna.nest.info`
/// reply (`NestInfoReply.capabilities`). A capability token is present iff this
/// nest's **build** includes the feature's kind-family — the I2 *version gate*
/// (`docs/goal/architecture/version-compatibility.md` § Dimension 3): it lets a
/// **newer** client tell whether an **older** nest is too old to *know* a feature,
/// so the client hides/disables it instead of letting the user invoke it and hit a
/// raw error.
///
/// **Three orthogonal gates, three homes — do not conflate:**
/// - `NestInfoReply.protocols` — federation/interop protocols (`"fauna"` + the
///   compile-time `nostr`/`bluesky`/`activitypub` bridges). Bridge UIs gate on
///   *that* field; capability tokens here deliberately do **not** duplicate them.
/// - `SetupStatusReply` runtime flags (`email_enabled`, …) — "the admin turned
///   it **on**". The finer runtime gate.
/// - `capabilities` (this set) — "this nest's **build** supports the feature at
///   all". The coarse version gate.
///
///   A client hides a feature when **either** its capability token is absent (nest
///   too old) **or** its runtime flag is off.
///
/// **Coarseness (anti-fingerprint, § Dimension 3 constraint):** tokens are coarse
/// feature names, never a patch/build fingerprint — the anonymous reply already
/// coarsens `version` to `major.minor` (`discovery_core.rs`).
///
/// **Additive-everywhere (I4):** a new feature appends a token when its kind-family
/// lands; nothing is ever removed/renamed within a major version. An old nest omits
/// tokens it predates; a client reads an omitted token as *unsupported* via
/// [`supports`] (the safe, non-erroring degrade — the `mail_subsystem_ok` skew
/// precedent, `nest/common.md`). Distinct from `pair::capability` (what a *linked
/// nest* may sync on the user's behalf) — that is a pairing grant, this is a nest
/// feature-support advertisement.
pub mod capability {
    /// Mail compose/inbox (`fauna.email.*` / `fauna.bridges.*` kind-families).
    /// Always present in this major; runtime availability is
    /// `SetupStatusReply.email_enabled`.
    pub const MAIL: &str = "mail";
    /// Calendar / CalDAV / events (`fauna.bridges.caldav.*`). Always present in
    /// this major; runtime availability is the admin `caldav_enabled` policy.
    pub const CALENDAR: &str = "calendar";
    /// Paid-tier subscriptions (`fauna.subscriptions.*`).
    ///
    /// **Conditional** since the nest-side `payments` excision (2026-08-10):
    /// advertised iff the nest build compiles the `payments` gated-feature
    /// member (`architecture/dynamic-features.md` § Wire & data shape), the
    /// `relay` shape minus its runtime half — payments has no runtime flag.
    /// Absent ⇒ the client hides its paid-tier surfaces and degrades exactly as
    /// against an older nest (*a peer without a capability, never a fork of the
    /// wire*). It previously read "always present in this major; no runtime
    /// flag, so this token is the sole support signal" — the first half is what
    /// changed; the second still holds, which is why the token is the signal to
    /// gate on rather than probing a kind.
    ///
    /// Note the token is about **money**, not about the whole kind family: a
    /// store-safe nest still serves the non-money `fauna.subscriptions.*` reads
    /// and still round-trips a tier's stored `asking_price`, because excision
    /// removes the ability to operate a feature and never the data at rest.
    pub const SUBSCRIPTIONS: &str = "subscriptions";
    /// File sync / backup (`fauna.filesync.*` / `fauna.sync.*` / `fauna.folders.*`).
    /// Always present in this major; no runtime flag.
    pub const FILE_SYNC: &str = "file_sync";

    /// The nest correctly **handles a client-sealed (opaque) per-user spam model
    /// at rest** — the spam-model client-write end-game's nest half
    /// (`mail-spam.md` § Encrypted-mode interaction): the per-user model rests
    /// ONLY sealed, `fauna.bridges.fetch_spam_model` returns the stored model
    /// **verbatim** (no seal-on-read double-seal), and the nest has no
    /// server-side model writer at all — every mutation is a capability
    /// holder's sealed `fauna.bridges.put_spam_model`, which refuses a plaintext
    /// model blob.
    ///
    /// **Load-bearing version gate (I1/I2):** the client-write surface
    /// MUST gate on this token before writing
    /// a sealed model via `fauna.bridges.put_spam_model`. `put_spam_model` landed
    /// one release **earlier**, *without* this handling — so a nest can accept the
    /// sealed write yet still seal-on-read, and a client that assumed the handling
    /// would double-seal the model into unreadability → cold-start reset =
    /// user-data loss. `put_spam_model`'s mere existence is therefore **not** the
    /// signal; this token is. Always-on in this major (the handling is
    /// unconditionally compiled — `bins/fauna-nest/src/spam_model_seal.rs::is_sealed_model_blob`).
    pub const SPAM_MODEL_SEALED_AT_REST: &str = "spam-model-sealed-at-rest";

    // RETIRED tokens — never reuse these names for a new capability:
    // `pq-hybrid` and `mail-epoch-schedule`. Both were advertised
    // unconditionally by every nest, so their absent-token degrade arms served
    // only a pre-sweep nest and were a downgrade lever; the compat-remnant sweep
    // (user-ruled 2026-09-24) struck them. Clients now publish the ML-KEM ek and
    // the epoch schedule unconditionally and seal hybrid whenever the recipient
    // published a post-quantum key (`architecture/security/post-quantum.md`
    // § Capability negotiation). A future suite gets a fresh token.

    /// P2P **relay** path availability — this nest can relay a P2P transport
    /// connection for the user's devices (`docs/goal/behavior/p2p.md`
    /// § Architecture — the substrate-agnostic transport seam negotiates "is a
    /// relay path available?" via this token, exactly as the design's "a
    /// capability the nest advertises + clients auto-negotiate"
    /// prescribes).
    ///
    /// Unlike the four always-on families above, this token
    /// is **NOT** part of the always-on baseline. Since the WireGuard stack's
    /// deletion (2026-08-23) it has exactly one backend: the self-hosted **iroh**
    /// relay sidecar (`bins/fauna-iroh-relay`), advertised only while one is
    /// connected to the nest. Advertising it without a serving
    /// backend would let a client discover a relay that isn't there — precisely the
    /// "invoke it and hit a raw error" failure the capability version-gate exists to
    /// prevent. The released image always runs the sidecar; a nest with no relay
    /// binary beside it (a desktop-served nest) never advertises `relay` →
    /// clients read it as unsupported via [`supports`] and use the
    /// always-available nest-mediated fallback. Running the sidecar is the
    /// artifact's decision, never a human knob (`p2p.md` § The relay).
    pub const RELAY: &str = "relay";

    /// Same-account **peer-leg sync** (device↔device store↔store sync over the
    /// P2P transport seam — `architecture/account-data-plane.md` § The peer
    /// leg). This token is the **fleet-wide version brake** the leg's
    /// wormability walk requires (rule 7): a client starts its peer-sync
    /// listener and dials siblings **only while its nest advertises this**, so
    /// pulling the token in a nest release is the emergency stop for a shipped
    /// peer-wire bug (accepted limit: a nest-unreachable fleet can't receive
    /// the brake — client update latency, the rule's own stated bound).
    ///
    /// The nest plays no part in the peer leg's traffic — the token is
    /// permission, not a nest feature — so it is **always-on baseline**: no
    /// client consumer exists yet (W3 (account-data-plane.md § Workstreams) assembles the first), and advertising
    /// now means those clients light up without a nest release. Never a human
    /// knob (the per-device participation choice is app surface on the `p2p`
    /// page; this is the fleet brake).
    pub const PEER_SYNC: &str = "peer-sync";

    /// Cross-user **shared-set transfer** — the share leg's fleet brake
    /// ([`../../../docs/goal/behavior/p2p.md`] § Cross-user shared-set transfer
    /// → *Wormability walk — the share leg*, rule 7). A client binds and serves
    /// the `fauna.peer.share.*` kinds **only** under this token, cached
    /// last-known so an offline start still decides — **no evidence refuses by
    /// default**. `libs/fauna-peer-share` deliberately ships no bind door for
    /// exactly this reason: whoever builds the listener owns checking the brake
    /// first.
    ///
    /// ⚠ **Conditional, unlike [`PEER_SYNC`] — and the asymmetry is
    /// deliberate.** `peer-sync` is always-on because the nest carries none of
    /// that leg's traffic, so the token is pure permission. The share plane has
    /// a **nest-side half**: rule 8's fan-out chokepoint
    /// (`p2p-share.member.admit`) is composed nest-side at the share roster
    /// write, because that is what holds against a non-conforming client — the
    /// structural anti-Pirate-Bay counterparty bound. A nest that compiled the
    /// plane away is not running that bound, so advertising the token would
    /// invite clients to bind a listener this box cannot lawfully police. It
    /// therefore follows [`SUBSCRIPTIONS`]: advertised iff the build compiles
    /// the member (`architecture/dynamic-features.md` § Wire & data shape;
    /// excision criterion 4).
    ///
    /// The constant itself is unconditional — only the *advertisement* is
    /// cfg-gated, exactly as `SUBSCRIPTIONS` is. Nothing here is a `fauna.peer.share.`
    /// kind string, so the store-safe witness's third column is untouched by it.
    /// Never a human knob: the per-device participation choice is app surface on
    /// the `p2p` page, and this is the fleet brake.
    pub const P2P_SHARE: &str = "p2p-share";

    /// Hidden subscription tiers are honored end to end (`monetization.md`
    /// § The unifying model → *A tier may be hidden*, ruled 2026-09-06): a
    /// `hidden` tier is excluded from every offer surface AND
    /// `fauna.subscriptions.subscribe` to it answers `tier_not_found`; the
    /// same build accepts the reserved-name `followers` `tiers.create` at
    /// rank 0 with a birth `KeyBlob`. The archive-import machine gates every
    /// non-public import on this token — against a nest without it the
    /// reserved owner-only tier would be created OFFERED (the flag lands in
    /// `extra` there), so the machine never mints it. Always-on in this major.
    pub const HIDDEN_TIERS: &str = "hidden-tiers";

    /// This nest serves `fauna.subscriptions.key_blob.rotate` — the
    /// roster-preserving re-key of a tier's client-minted period key
    /// (`succession-aftermath.md` § Re-key scope, the tier row).
    ///
    /// **The token is the version brake on a leg that must not half-run.** The
    /// post-succession rotation persists a fresh period key into `current`
    /// *before* uploading the covering blob, so against a nest that does not
    /// serve the kind the successor would rotate into a key no subscriber can
    /// unwrap and then meet a bare unknown-kind error with no way to tell a
    /// nest without the door from an outage. Gating on the token makes that pass a clean
    /// no-op instead: the tier stays on the predecessor-era key — still the
    /// exposure the leg exists to close, but visible, stated and closed by
    /// updating the nest, rather than a broadcast plane nobody can read.
    ///
    /// Always-on in this major, like [`HIDDEN_TIERS`] and for its reason: this
    /// is key management, not money, so it keeps working in a payments-excised
    /// store-safe build and must not ride [`SUBSCRIPTIONS`]' conditional
    /// advertisement. Never a human knob.
    pub const SUBSCRIPTION_PERIOD_ROTATE: &str = "subscription-period-rotate";

    /// Whether `token` is advertised in a nest's `NestInfoReply.capabilities`.
    ///
    /// The degrade contract: a token the nest does **not** advertise (including an
    /// older nest that omits the whole `capabilities` field — it decodes to an empty
    /// `Vec` via `#[serde(default)]`) reads as **`false` = unsupported**. This is the
    /// safe, non-erroring default for capability semantics — the same "an omitted
    /// field never errors" principle as `mail_subsystem_ok` (`nest/common.md`), just
    /// with the value that fits *this* meaning (unsupported, hide the feature) rather
    /// than `mail_subsystem_ok`'s `true`.
    pub fn supports(capabilities: &[String], token: &str) -> bool {
        capabilities.iter().any(|c| c == token)
    }
}

// ── fauna.nest.info (≡ GET /api/v1/node-info) ───────────────────────────────

/// Request the nest's public metadata. No input — the empty map mirrors
/// `SpamGetPreferencesRequest`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NestInfoRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Public node metadata: domain, nest identity, supported protocols, and the
/// registration / moderation policy.
///
/// `Default` exists so fixtures can be written struct-update style
/// (`NestInfoReply { domain: …, ..Default::default() }`) — this type grows
/// additively within a major version, and hand-listing every field makes two
/// branches that each add one collide on the grown axis.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NestInfoReply {
    /// The nest's handle domain (`"unknown"` when unset).
    ///
    /// ⚠ **Not the domain a client composes web-serving URLs on** — that is
    /// [`Self::web_serving_domain`]. This field is the *identity* handle
    /// domain — the live claimed domain first, the registration seed only on
    /// a never-claimed box (`AppState::handle_domain_if_set`'s chain) — and
    /// carries the `"unknown"` placeholder when neither is set. It is the
    /// nest's own claim about itself: a reader that must *bind* a domain to
    /// this nest's key resolves from the domain end instead
    /// (`federation.md` § Security → *Domain↔key binding rides TLS*).
    pub domain: String,
    /// 64-char hex of the nest's 32-byte Ed25519 public key.
    pub nest_id: String,
    /// `CARGO_PKG_VERSION` of the running nest.
    pub version: String,
    /// Software identifier — always `"fauna"`.
    pub software: String,
    /// Federation protocols this build speaks (`"fauna"` plus any of
    /// `"nostr"`/`"bluesky"`/`"activitypub"` enabled at compile time). Bridge UIs
    /// gate on this field; it is *not* the feature-capability set (`capabilities`).
    pub protocols: Vec<String>,
    /// Build/version feature-capability tokens (see [`capability`]) — which
    /// fauna-native feature-families this nest's *build* supports, so a newer
    /// client can hide a feature an older nest predates. Coarse (anti-fingerprint),
    /// additive-everywhere. `#[serde(default)]`: an older nest that predates this
    /// field omits it, decoding to `vec![]` — every token then reads as
    /// *unsupported* via [`capability::supports`] (the `mail_subsystem_ok` skew
    /// precedent). Distinct from `protocols` (federation) and from
    /// `SetupStatusReply` runtime flags ("admin enabled it").
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Public URL of this nest's self-hosted iroh P2P relay
    /// (`https://relay.<domain>`), advertised alongside the `relay` capability so a
    /// client behind NAT can dial it via `RelayMode::Custom` when no direct path
    /// establishes (`fauna_iroh::IrohTransport`). `None` (the default) on a nest
    /// with no iroh relay — including a WireGuard-only relay, which still advertises
    /// the `relay` capability but routes via the peer registry, so a present `relay`
    /// capability with `iroh_relay_url == None` is a valid state the client handles
    /// by falling back. `#[serde(default)]`: `None` when no iroh relay is configured.
    #[serde(default)]
    pub iroh_relay_url: Option<String>,
    /// Whether per-handle subdomain addressing is enabled (DNS-managed).
    pub subhandles: bool,
    /// Registration policy — `None` when the nest has no handle domain (the
    /// HTTP twin's `registration: null`).
    pub registration: Option<RegistrationInfo>,
    /// Moderation policy summary.
    pub moderation: ModerationInfo,
    /// **The domain this nest's web `HostResolver` actually routes user content
    /// on** — the one value a client may compose a `<handle>.<domain>` site URL
    /// from. Empty string ⇒ this nest serves no subdomain/apex user content at
    /// all (a domainless localhost/IP box), so every copy affordance must
    /// disable with a reason rather than hand out a link that cannot load
    /// (`web-content-hosting.md` § Published-post management, *"publishing with
    /// no serving origin is legal but unreachable — the UI must say so"*).
    ///
    /// ⚠ **This is deliberately none of the three values a client might guess**,
    /// each of which produces a dead link on some real deployment:
    /// - the address the client **dialed** — wrong whenever a nest is reached by
    ///   IP or by any name other than its serving domain;
    /// - the sign-in reply's `domain` (`AppState::handle_domain`) — same chain,
    ///   but with a **`"localhost"` placeholder** on a domainless box, and
    ///   `.localhost` is precisely the suffix the resolver never strips;
    /// - [`RegistrationInfo::handle_domain`] / [`Self::domain`] — the *boot seed*,
    ///   which a box that learned its domain at claim has already moved past.
    ///
    /// Mirrors `AppState::web_serving_domain()` (identity domain claimed at
    /// runtime → registration handle domain → node domain → empty), which is the
    /// same accessor `HostResolver` and the per-subdomain cert loop key off.
    ///
    /// `#[serde(default)]`: an absent key decodes to `None`, which clients read
    /// as this nest serving nothing.
    #[serde(default)]
    pub web_serving_domain: Option<String>,
    /// Hex of this nest's **room-read** X-Wing reception public key — the wrap
    /// target that makes it a readable member of the community rooms it homes
    /// (`../../docs/goal/architecture/key-material-hierarchy.md` § Audience:
    /// deployment infrastructure → *Room-read keypair*).
    ///
    /// Published here rather than behind a kind of its own because that is
    /// what it is: a **public key of the deployment**, exactly like
    /// [`Self::nest_id`] beside it. An owner or admin device that is making
    /// this nest a member of a room needs it *before* the membership exists,
    /// so a membership-gated read would be a chicken-and-egg; and it discloses
    /// nothing — a KEM public key is what you must hand out to be sealed to.
    ///
    /// `#[serde(default)]` → `None` when the nest has no signing key or an
    /// unreadable row, which reads correctly as *this nest cannot be a
    /// community room's reader*: it holds no room-read key, so a client must not offer
    /// to home a community room there. The same shape an absent capability
    /// takes.
    #[serde(default)]
    pub room_read_pubkey: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Self-service registration facts (present only on nests with a handle domain).
/// The registration *posture* is `SetupStatusReply::registration_mode`; the
/// legacy `open` / `invite_required` boolean projection of it left this block
/// with the compat-remnant sweep (`version-compatibility.md` § Dimension 2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegistrationInfo {
    /// Tier names available at registration (defaults to `["free"]`).
    pub tiers: Vec<String>,
    /// The handle domain new handles are minted under.
    pub handle_domain: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Moderation policy summary.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModerationInfo {
    // No field today: the nest carries no scanning obligations for clients (the
    // in-nest classifier stack was retired, content-scoring.md § Architectural
    // rules). The struct is the slot a moderation summary grows into.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.handle.available (≡ GET /api/v1/handle-available/{handle}) ─────────

/// Check whether `handle` is free before registering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HandleAvailableRequest {
    /// Bare handle (no domain). Validated per the registration handle rules;
    /// an invalid handle is rejected with `fauna.handle.invalid`.
    pub handle: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HandleAvailableReply {
    /// Whether the handle can be claimed right now.
    pub available: bool,
    /// Echo of the requested handle.
    pub handle: String,
    /// The nest's handle domain.
    pub domain: String,
    /// `true` when the handle is unavailable specifically because it is in
    /// release cooldown for a different actor (the HTTP twin's optional
    /// `cooldown: true`); always present here, default `false`.
    pub cooldown: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.nest.resolve (≡ GET /api/v1/resolve-node/{domain}) ─────────────────

/// Resolve a domain to its canonical fauna node URL via SRV.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NestResolveRequest {
    /// The domain to resolve. Bare IPs, `localhost`, and `.local`/`.internal`
    /// domains are rejected with `fauna.nest.invalid_domain`.
    pub domain: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NestResolveReply {
    /// The resolved full node URL (the HTTP twin's `{"url": …}`).
    pub url: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.actor.by_handle (≡ GET /api/v1/actor/by-handle/{handle}) ───────────

/// Resolve a human-readable handle to an actor ID for addressing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActorByHandleRequest {
    /// The handle to resolve. An unknown handle is rejected with
    /// `fauna.actor.not_found`.
    pub handle: String,
    /// Optional domain qualifier for multi-domain handles. When a handle
    /// reference names one of the deployment's active local domains
    /// (`@bob@domain2`), the resolver echoes that domain in the reply so a
    /// client can display `bob@domain2`; a named domain the nest does **not**
    /// serve is rejected (`fauna.actor.domain_not_local`). Absent → the reply
    /// reports the deployment's canonical identity domain (today's single-domain
    /// behavior, unchanged). Additive (`mail-multidomain.md` § Multi-domain
    /// handles § Resolution); absent means the canonical domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActorByHandleReply {
    /// 64-char hex of the resolved 32-byte actor public key.
    pub actor_id: String,
    /// Echo of the requested handle.
    pub handle: String,
    /// The nest's handle domain.
    pub domain: String,
    /// Subhandle addresses (`handle@domain`, `@handle.domain`); empty when
    /// subhandles are disabled (the HTTP twin's optional `addresses`).
    pub addresses: Vec<String>,
    /// Spec Y2 reachability probe: whether the actor currently has at least one
    /// **usable** key package — one-time **or** last-resort. Folded into the
    /// already-anonymous discovery reply so a picker can pre-flight messageability
    /// without a separate cross-nest call; it leaks **yes/no, not a number**.
    /// Because every actor publishes a last-resort key package at onboarding,
    /// `addressable` is ~equivalent to "this handle exists" — which a successful
    /// `by_handle` already revealed — so it conveys no new information
    /// (`docs/goal/architecture/federation.md` § Key packages). Absent
    /// reads as `false`.
    #[serde(default)]
    pub addressable: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Whether a `fauna.actor.by_handle` rejection means **the handle does not
/// resolve here** — no such handle (`fauna.actor.not_found`), or not a handle
/// at all (`fauna.handle.invalid`) — as opposed to a fault the caller must
/// surface.
///
/// One statement of the split, because every caller that falls through a
/// resolution chain on "no such handle" makes it: the recipient picker's
/// `ConversationsClient::actor_by_handle` (which maps these to `Ok(None)`), and
/// the followed-folder owner check that re-verifies a remembered handle
/// (`fauna_client_folders::public_follow::verify_owner_handle`).
pub fn is_unresolved_handle_code(code: &str) -> bool {
    code == "fauna.actor.not_found" || code == "fauna.handle.invalid"
}

// ── fauna.setup.status (≡ GET /api/v1/setup-status) ─────────────────────────

/// Request the setup wizard's progress. No input.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SetupStatusRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One provisioned DKIM record's public half — the value an admin publishes as
/// the `<selector>._domainkey.<domain>` TXT record (`v=DKIM1; k=ed25519;
/// p=<base64>`). Surfaced on the anonymous [`SetupStatusReply`] so a
/// credential-less deploy-verify gate can compare the *published* DNS record
/// against the nest's *current* signing key and catch a stale publish — DKIM has
/// no DNS auto-reconcile, so a key re-mint silently orphans the published TXT
/// (the 2026-06-21 `dkim=fail` class; `mail-bridge-lifecycle.md` § DKIM
/// provisioning). Public by design: the value is meant to live in public DNS and
/// `public_dns_value` carries no key material (the sealed private key is openable
/// only with the MTA bridge's X25519 secret, which no client holds).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DkimRecord {
    /// The mail domain this selector signs for.
    pub domain: String,
    /// The DKIM selector — the `<selector>` label of `<selector>._domainkey`
    /// (defaults to `"default"`).
    pub selector: String,
    /// The full TXT record body the nest's CURRENT signing key requires
    /// (`v=DKIM1; k=ed25519; p=<base64>`), read straight from
    /// `mail_dkim_keys.public_dns_value` — authoritative over what must be
    /// published.
    pub public_dns_value: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Setup-wizard progress. The HTTP twin nests `dns`/`tls`/`email`/`admin` as
/// single-field objects; the typed wire flattens them to booleans (same
/// information).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetupStatusReply {
    /// The configured node domain (`"unknown"` when unset).
    pub domain: String,
    /// Whether a DNS manager is configured (HTTP `dns.configured`).
    pub dns_configured: bool,
    /// Whether TLS is active (HTTP `tls.active`).
    pub tls_active: bool,
    /// Whether email is enabled (HTTP `email.enabled`).
    pub email_enabled: bool,
    /// Whether any user exists yet (HTTP `admin.exists`).
    pub admin_exists: bool,
    /// Whether the admin claim code has been consumed.
    pub claimed: bool,
    /// The nest's resolved **NAT axis** — `"public"` / `"private"` (the
    /// lowercase `NodeMode` wire form): the client-set `nest_nat_mode` row
    /// falling back to the `FAUNA_MODE` config seed
    /// (`nat_mode_core::resolve_node_mode`). Seeds the `nat_mode_choice`
    /// wizard pre-selection so the page is confirm-only in the common case
    /// (`docs/goal/behavior/onboarding.md` § 3b-bis); the admin changes it
    /// via the mutable `fauna.setup.nat_mode` kind. Always resolved (an unset
    /// row falls back to the seed), so always present.
    pub node_mode: String,
    /// `CARGO_PKG_VERSION` of the running nest.
    pub version: String,
    /// Whether the mail subsystem's nest-side config query is healthy. `false`
    /// means email is enabled but the query the mail bridge boots from
    /// (`list_active_mail_domains`) is failing — e.g. a schema break that
    /// crash-loops the bridge while `/api/v1/health` still says `ok` (the
    /// outage class). `true` when mail is disabled (nothing to
    /// break) or the query succeeds. Defaults to `true` when absent (treat
    /// as "no problem reported", not a false alarm).
    #[serde(default = "default_true")]
    pub mail_subsystem_ok: bool,
    /// Deployment policy: whether a freshly-registered user's client
    /// auto-provisions its own mailbox on first setup (the works-out-of-box
    /// invariant, extended to every user). Default-**on**; the admin disables it
    /// deployment-wide via `fauna.bridges.set_auto_enable_mail_for_new_users`.
    /// The client gates the first-setup auto-mint on this **and** `email_enabled`
    /// — the nest cannot mint the mailbox (the MSEK is client-held). NOT a
    /// per-user control (mail is user-controlled — `admin.md` § Don't do these).
    /// Defaults to `true` when absent (preserve the out-of-box default
    /// rather than silently disabling it).
    #[serde(default = "default_true")]
    pub auto_enable_mail_for_new_users: bool,
    /// Deployment policy: the client-set **registration posture** — the wire string
    /// of [`crate::node_policy::RegistrationMode`] (`open` / `invite_required` /
    /// `closed`). The admin sets it via `fauna.admin.set_registration_mode` and
    /// renders it on the `admin-users` registration section.
    ///
    /// Every nest reports it; `#[serde(default)]` is the ordinary decode
    /// discipline, and a client renders an absent or unrecognised value
    /// read-only rather than guessing a posture. (The retired
    /// `require_registration` flag and node-info's `open` / `invite_required`
    /// booleans left the wire with the compat-remnant sweep.)
    #[serde(default)]
    pub registration_mode: Option<String>,
    /// Deployment policy: the client-set free-tier ceiling. `None` ⇒ no cap.
    /// Orthogonal to `registration_mode` (it applies "regardless of mode").
    #[serde(default)]
    pub max_free_users: Option<u64>,
    /// Deployment policy: the client-set `subhandles` gate (whether the nest
    /// advertises the `handle@domain` / `@handle.domain` address forms). The admin
    /// flips it via `fauna.admin.set_subhandles`; also reflected on
    /// `fauna.nest.info`. Default-**off** when absent.
    #[serde(default)]
    pub subhandles: bool,
    /// Deployment policy: the client-set **"accept only signups carrying app
    /// age verification"** gate (`family-safety.md` § The account age band,
    /// D5+D6; gating scope owned by `public-mode.md` § Registration Modes →
    /// *Age at registration* — it refuses self-service admissions with no
    /// verified attested claim). The admin flips it via
    /// `fauna.admin.set_age_verification_required`. Default-**off** when
    /// absent (the works-out-of-the-box default — web/desktop signups have no
    /// attestation path).
    #[serde(default)]
    pub age_verification_required: bool,
    /// Deployment policy: the client-set node-wide storage/capacity cap, in bytes.
    /// `Some(v)` caps the nest at `v` bytes; `None` means no cap (the default). The admin sets/clears it via
    /// `fauna.admin.set_max_storage_bytes`; surfaced here so the admin client
    /// reads the current value back.
    #[serde(default)]
    pub max_storage_bytes: Option<u64>,
    /// Deployment policy: the client-set list of browser origins the nest's own
    /// client-facing HTTP API trusts for credentialed cross-origin requests. The
    /// admin sets/clears it via `fauna.admin.set_cors_origins`; surfaced here so
    /// the admin client renders the current list back. Empty (the default) means the nest trusts only its
    /// built-in default origin.
    #[serde(default)]
    pub cors_origins: Vec<String>,
    /// Deployment policy: the admin's chosen client-facing API serving port (the
    /// nest's own HTTPS listener — the WS-RPC transport + the served SPA). The
    /// admin sets it via `fauna.admin.set_serving_port`; surfaced here so the
    /// admin client reads the current value back. This is the **value** the admin
    /// chose, defaulting to [`crate::node_policy::DEFAULT_SERVING_PORT`] (443) when
    /// unset — the uniform client-facing port, *not* the per-deployment internal
    /// bind seed (e.g. `3000` behind the SNI router). Defaults to 443 when absent. Realization is per-deployment
    /// (direct-listener bind vs. provisioning orchestrator) — see
    /// `architecture/nest/common.md` § Serving ports.
    #[serde(default = "default_serving_port")]
    pub serving_port: u16,
    /// Deployment wiring: whether this nest sits **behind the SNI router**
    /// (`fauna-sni-router` owns the external `:443`) — `true` on every Docker/cloud
    /// image, `false` on a direct-listener desktop / self-hosted / bare-IP nest.
    /// When `true` the chosen [`serving_port`](Self::serving_port) is **inert** for
    /// nest's own bind (the external port is the router's fixed `443`) and
    /// `fauna.admin.set_serving_port` is **rejected** (`serving_port_fronted`), so
    /// the admin client renders the `admin-nest-serving-port` field **read-only**
    /// ("served on 443 by this deployment") instead of offering a doomed save. NOT
    /// an admin choice — it is bucket-2 artifact-wiring (`FAUNA_FRONTED_BY_ROUTER`),
    /// surfaced only so the UI reflects whether the port is settable on *this*
    /// deployment. Defaults to `false` when absent (a direct-listener) — i.e. "settable", which matches the common
    /// admin-choice case and never wrongly locks a genuinely settable field. See
    /// `architecture/nest/common.md` § Serving ports.
    #[serde(default)]
    pub fronted_by_router: bool,
    /// The nest's CURRENT DKIM records — one per provisioned `(domain, selector)`,
    /// each carrying the `public_dns_value` (TXT body) the live signing key
    /// requires. Surfaced so a credential-less deploy-verify gate can assert the
    /// published `<selector>._domainkey.<domain>` TXT matches the live key: DKIM
    /// has no DNS auto-reconcile, so a key re-mint silently orphans the published
    /// record while SPF/DMARC still pass (the 2026-06-21 `dkim=fail` miss;
    /// `mail-bridge-lifecycle.md` § DKIM provisioning). Public-by-design (the
    /// values live in public DNS; no key material — see [`DkimRecord`]). Empty
    /// when DKIM is not provisioned.
    #[serde(default)]
    pub dkim_records: Vec<DkimRecord>,
    /// Host-OS maintenance: the number of pending **security** updates on the
    /// host Ubuntu box (an onboarded VPS — `installers/vps.md` § Host OS
    /// Maintenance). The host's `fauna-reboot-coordinator` writes the live count
    /// into the root-owned `/data/maintenance-host` `:ro` bind mount; the nest
    /// reads it and surfaces it so the admin client can show "N security updates
    /// pending". `0` (also the
    /// value any nest without the maintenance mount — dev /
    /// desktop / bare-metal — reports) means nothing is pending. Authority:
    /// `installers/vps.md` § Host OS Maintenance § 4.
    #[serde(default)]
    pub os_security_updates_pending: u32,
    /// Host-OS maintenance: whether the host has a pending kernel/`glibc`/`systemd`
    /// update that needs a reboot (`/run/reboot-required`). The nest reboots the
    /// box only when idle (or past a 24 h ceiling); the admin client shows
    /// "Restart pending — will restart automatically when idle". `false` (also the
    /// value a nest without the mount reports) means no reboot
    /// is pending. See [`Self::os_security_updates_pending`].
    #[serde(default)]
    pub os_reboot_pending: bool,
    /// Host-OS maintenance: the unix-seconds instant the pending reboot was first
    /// observed (drives the 24 h hard ceiling). `None` when no reboot is pending
    /// or the nest lacks the maintenance channel. See
    /// [`Self::os_reboot_pending`].
    #[serde(default)]
    pub os_reboot_deferred_since: Option<i64>,
    /// Host-OS maintenance: the unix-seconds instant `unattended-upgrades` last
    /// applied patches on the host (the `unattended-upgrades-stamp` mtime). `None`
    /// when never patched yet or the nest lacks the maintenance channel.
    /// See [`Self::os_security_updates_pending`].
    #[serde(default)]
    pub os_last_patched_at: Option<i64>,
    /// Deployment policy: the admin's choice of what this nest's `/app/`
    /// answers — the wire spelling of
    /// [`crate::web_app_origin::WebAppOrigin`] (`bundled` / `central`), set via
    /// `fauna.admin.web_app_origin.set`; always present (an unset choice
    /// reads `bundled`). The three `web_app_origin*` fields are
    /// [`crate::web_app_origin::AdminWebAppOriginGetReply::project`]'s, so the
    /// status text and the admin read can never disagree.
    pub web_app_origin: String,
    /// The exact address a user who opens this nest's `/app/` is sent to;
    /// `None` when `/app/` serves the bundled SPA.
    #[serde(default)]
    pub web_app_origin_target: Option<String>,
    /// The choice is central but this nest has no handle domain, so `/app/`
    /// serves bundled regardless.
    #[serde(default)]
    pub web_app_origin_domainless: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Serde default for [`SetupStatusReply::mail_subsystem_ok`] — a missing field
/// means "unknown", which for a health signal is safest read as
/// healthy so an absent key never raises a phantom alarm.
fn default_true() -> bool {
    true
}

/// Serde default for [`SetupStatusReply::serving_port`] — a missing field
/// reports the hard-coded default client-facing port (443), the value a
/// nest serves on when the admin has never chosen otherwise.
fn default_serving_port() -> u16 {
    crate::node_policy::DEFAULT_SERVING_PORT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn nest_info_request_round_trips_from_empty_map() {
        // The client sends an empty map `{}` (mirrors SpamGetPreferencesRequest).
        let req = NestInfoRequest::default();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: NestInfoRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn nest_info_reply_round_trips_with_registration() {
        let reply = NestInfoReply {
            domain: "nest.example".into(),
            nest_id: "ab".repeat(32),
            version: "0.1.0".into(),
            software: "fauna".into(),
            protocols: vec!["fauna".into(), "nostr".into()],
            capabilities: vec!["mail".into(), "calendar".into(), "relay".into()],
            iroh_relay_url: Some("https://relay.nest.example".into()),
            subhandles: true,
            registration: Some(RegistrationInfo {
                tiers: vec!["free".into(), "personal".into()],
                handle_domain: Some("nest.example".into()),
                extra: BTreeMap::new(),
            }),
            moderation: ModerationInfo {
                extra: BTreeMap::new(),
            },
            web_serving_domain: Some("nest.example".into()),
            ..Default::default()
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: NestInfoReply = decode(&bytes1).unwrap();
        assert_eq!(reply, decoded);
        // Canonical re-encode is stable.
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn nest_info_reply_round_trips_without_registration() {
        let reply = NestInfoReply {
            domain: "unknown".into(),
            nest_id: "00".repeat(32),
            version: "0.1.0".into(),
            software: "fauna".into(),
            protocols: vec!["fauna".into()],
            capabilities: vec![],
            iroh_relay_url: None,
            subhandles: false,
            registration: None,
            moderation: ModerationInfo {
                extra: BTreeMap::new(),
            },
            // A domainless box: the registration domain reads `"unknown"`, and
            // the serving domain is the EMPTY string — "this nest serves no
            // user web content", which is a different statement from "absent".
            web_serving_domain: Some(String::new()),
            ..Default::default()
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: NestInfoReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
        assert!(decoded.registration.is_none());
        assert!(decoded.iroh_relay_url.is_none());
        assert_eq!(
            decoded.web_serving_domain.as_deref(),
            Some(""),
            "an empty serving domain must survive the round-trip as Some(\"\") — a \
             nest that answered \"I serve nothing\" is not the same as an absent \
             key (None), though a client reads both as serving nothing"
        );
    }

    /// Skew degrade (I2 backward-compat): a reply without the `capabilities` /
    /// `iroh_relay_url` fields omits those keys
    /// entirely; a newer client decoding it must see `vec![]` / `None` (via
    /// `#[serde(default)]`) and read every capability as *unsupported* and the relay
    /// URL as absent — never a decode error. We simulate the reply with a
    /// field-less mirror struct (its canonical bytes carry neither key), exactly the
    /// `test_wire_version_skew.py` skew-simulator pattern at the type level.
    #[test]
    fn nest_info_reply_from_old_nest_without_capabilities_reads_unsupported() {
        /// The `NestInfoReply` shape of a reply without `capabilities` — no
        /// `capabilities` field.
        #[derive(Serialize)]
        struct OldNestInfoReply {
            domain: String,
            nest_id: String,
            version: String,
            software: String,
            protocols: Vec<String>,
            subhandles: bool,
            registration: Option<RegistrationInfo>,
            moderation: ModerationInfo,
        }
        let old = OldNestInfoReply {
            domain: "old.example".into(),
            nest_id: "11".repeat(32),
            version: "0.1".into(),
            software: "fauna".into(),
            protocols: vec!["fauna".into()],
            subhandles: false,
            registration: None,
            moderation: ModerationInfo {
                extra: BTreeMap::new(),
            },
        };
        let bytes = encode_canonical(&old).unwrap();
        let decoded: NestInfoReply = decode(&bytes).unwrap();
        assert!(
            decoded.capabilities.is_empty(),
            "a reply omitting `capabilities` must decode to an empty Vec, not error"
        );
        assert!(
            !capability::supports(&decoded.capabilities, capability::MAIL),
            "a capability the (old) nest never advertised reads as unsupported"
        );
        assert!(
            decoded.iroh_relay_url.is_none(),
            "a reply omitting `iroh_relay_url` must decode to None, not error"
        );
        assert!(
            decoded.web_serving_domain.is_none(),
            "an absent `web_serving_domain` must decode to None, not \
             error — `None` reads as serving nothing, and must stay \
             distinguishable from `Some(\"\")`"
        );
        assert!(
            decoded.room_read_pubkey.is_none(),
            "a reply omitting `room_read_pubkey` must decode to None, not error \
             — and `None` reads as *this nest cannot be a community room's reader*, \
             which is the honest answer for a nest that holds no room-read keypair"
        );
    }

    #[test]
    fn capability_supports_membership() {
        let caps = vec![
            capability::MAIL.to_string(),
            capability::SUBSCRIPTIONS.to_string(),
        ];
        assert!(capability::supports(&caps, capability::MAIL));
        assert!(capability::supports(&caps, capability::SUBSCRIPTIONS));
        // A token the nest does not advertise (here: a feature this nest predates,
        // or a future capability a newer client knows of) reads as unsupported.
        assert!(!capability::supports(&caps, capability::CALENDAR));
        assert!(!capability::supports(
            &caps,
            "fauna.future.not_a_real_capability"
        ));
    }

    /// The `relay` token follows the standard absent ⇒ unsupported degrade: a nest
    /// running the iroh relay sidecar dormant (the shipping default) does not
    /// advertise it, so a client reads "no relay path" and uses the nest-mediated
    /// fallback. An artifact that enables the sidecar advertises it and the same
    /// check reads supported.
    #[test]
    fn relay_capability_absent_reads_unsupported() {
        assert_eq!(capability::RELAY, "relay");
        let no_relay = vec![
            capability::MAIL.to_string(),
            capability::CALENDAR.to_string(),
            capability::SUBSCRIPTIONS.to_string(),
            capability::FILE_SYNC.to_string(),
        ];
        assert!(!capability::supports(&no_relay, capability::RELAY));
        let mut with_relay = no_relay.clone();
        with_relay.push(capability::RELAY.to_string());
        assert!(capability::supports(&with_relay, capability::RELAY));
    }

    #[test]
    fn handle_available_round_trips() {
        let req = HandleAvailableRequest {
            handle: "alice".into(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(req, decode::<HandleAvailableRequest>(&bytes).unwrap());

        let reply = HandleAvailableReply {
            available: false,
            handle: "alice".into(),
            domain: "nest.example".into(),
            cooldown: true,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(reply, decode::<HandleAvailableReply>(&bytes).unwrap());
    }

    #[test]
    fn nest_resolve_round_trips() {
        let req = NestResolveRequest {
            domain: "example.com".into(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(req, decode::<NestResolveRequest>(&bytes).unwrap());

        let reply = NestResolveReply {
            url: "https://example.com:8443/api".into(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(reply, decode::<NestResolveReply>(&bytes).unwrap());
    }

    #[test]
    fn actor_by_handle_round_trips_with_and_without_addresses() {
        let req = ActorByHandleRequest {
            handle: "alice".into(),
            domain: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(req, decode::<ActorByHandleRequest>(&bytes).unwrap());

        // The additive `domain` qualifier round-trips when present.
        let req_dom = ActorByHandleRequest {
            handle: "alice".into(),
            domain: Some("domain2.example".into()),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req_dom).unwrap();
        assert_eq!(req_dom, decode::<ActorByHandleRequest>(&bytes).unwrap());

        let with = ActorByHandleReply {
            actor_id: "cd".repeat(32),
            handle: "alice".into(),
            domain: "nest.example".into(),
            addresses: vec!["alice@nest.example".into(), "@alice.nest.example".into()],
            addressable: true,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&with).unwrap();
        assert_eq!(with, decode::<ActorByHandleReply>(&bytes).unwrap());

        let without = ActorByHandleReply {
            addresses: vec![],
            addressable: false,
            ..with
        };
        let bytes = encode_canonical(&without).unwrap();
        assert_eq!(without, decode::<ActorByHandleReply>(&bytes).unwrap());
    }

    #[test]
    fn setup_status_round_trips() {
        let req = SetupStatusRequest::default();
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(req, decode::<SetupStatusRequest>(&bytes).unwrap());

        let chosen = SetupStatusReply {
            domain: "nest.example".into(),
            dns_configured: true,
            tls_active: true,
            email_enabled: false,
            admin_exists: true,
            claimed: true,
            // Proves the resolved NAT axis rides the wire so `nat_mode_choice`
            // can pre-select it.
            node_mode: "public".into(),
            version: "0.1.0".into(),
            // Non-default (the serde default is `true`) so the round-trip proves
            // the flag is actually carried on the wire, not silently defaulted.
            mail_subsystem_ok: false,
            // Same — non-default (serde default `true`) proves the policy flag
            // rides the wire and isn't silently re-defaulted on decode.
            auto_enable_mail_for_new_users: false,
            registration_mode: None,
            max_free_users: None,
            // Non-default (serde default `false`) — proves it rides the wire.
            subhandles: true,
            // Non-default (serde default `false`) — proves the age-verification
            // require-knob rides the wire for the admin read-back.
            age_verification_required: true,
            // Non-default (serde default `None`) — proves the cap rides the wire.
            max_storage_bytes: Some(8_000_000_000),
            // Non-default (serde default empty) — proves the list rides the wire.
            cors_origins: vec!["https://app.example.com".into()],
            // Non-default (serde default 443) — proves the chosen port rides the wire.
            serving_port: 3443,
            // Non-default (serde default `false`) — proves the fronted flag rides the
            // wire so the client can gate the serving-port field read-only.
            fronted_by_router: true,
            // Non-default (serde default empty) — proves the DKIM records ride the
            // wire so the credential-less deploy-verify gate can read them.
            dkim_records: vec![DkimRecord {
                domain: "nest.example".into(),
                selector: "default".into(),
                public_dns_value: "v=DKIM1; k=ed25519; p=AAAA".into(),
                extra: BTreeMap::new(),
            }],
            // Non-default host-OS-maintenance fields (serde defaults are 0 /
            // false / None) — prove the os_* values ride the wire so the admin
            // client can render the patch/reboot indicator.
            os_security_updates_pending: 3,
            os_reboot_pending: true,
            os_reboot_deferred_since: Some(1_719_500_000),
            os_last_patched_at: Some(1_719_400_000),
            web_app_origin: "central".into(),
            web_app_origin_target: Some("https://app.fauna.social/app/?nest=nest.example".into()),
            web_app_origin_domainless: true,
            extra: BTreeMap::new(),
        };
        let bytes1 = encode_canonical(&chosen).unwrap();
        let decoded: SetupStatusReply = decode(&bytes1).unwrap();
        assert_eq!(chosen, decoded);
        assert_eq!(decoded.os_security_updates_pending, 3);
        assert!(decoded.os_reboot_pending);
        assert_eq!(decoded.os_reboot_deferred_since, Some(1_719_500_000));
        assert_eq!(decoded.os_last_patched_at, Some(1_719_400_000));
        assert!(!decoded.auto_enable_mail_for_new_users);
        assert!(decoded.subhandles);
        assert_eq!(decoded.max_storage_bytes, Some(8_000_000_000));
        assert_eq!(decoded.cors_origins, vec!["https://app.example.com"]);
        assert_eq!(decoded.serving_port, 3443);
        assert!(decoded.fronted_by_router);
        assert_eq!(decoded.dkim_records.len(), 1);
        assert_eq!(decoded.dkim_records[0].selector, "default");
        assert_eq!(
            decoded.dkim_records[0].public_dns_value,
            "v=DKIM1; k=ed25519; p=AAAA"
        );
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);

        // A reply carrying the retired storage `mode` key (any unknown key) is
        // absorbed by the catch-all, never refused.
        let mut map: BTreeMap<String, Value> = decode(&bytes1).unwrap();
        map.insert("mode".into(), Value::String("Encrypted".into()));
        let decoded: SetupStatusReply = decode(&encode_canonical(&map).unwrap()).unwrap();
        assert_eq!(
            decoded.extra.get("mode"),
            Some(&Value::String("Encrypted".into()))
        );
    }
}
