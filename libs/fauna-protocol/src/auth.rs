//! Pre-identity auth-bootstrap WS-RPC payload types — `fauna.auth.{handshake,
//! challenge,verify}`. A behavior-preserving transport migration of the HTTP
//! routes `POST /api/v1/auth/{token,challenge,verify}`
//! (`bins/fauna-nest/src/routes.rs` + `challenge_auth.rs`); the request/reply
//! fields, signed-message construction, drift window, and side effects are
//! preserved exactly — only HTTP-JSON moves to dag-cbor WS-RPC. These kinds run
//! on the **anonymous** WS connection (`GET /api/v1/ws`, no bearer) specified in
//! `docs/goal/architecture/transport.md` § Pre-identity (anonymous) connection.
//!
//! `actor_id` / `nonce` / `signature` are **hex-encoded `String`** — matching
//! the HTTP twin and the established wire convention (`conversations.rs`,
//! `segments.rs`, `posts.rs`: identity references are hex strings; only opaque
//! payloads ride as CBOR `bstr`). The opaque bearer `token` is genuinely a
//! string. Slice scoped as part of the WS-RPC pre-identity migration
//! (tracked internally).
//!
//! Kind registry: `bins/fauna-nest/src/auth_handlers.rs::register_auth_handlers`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{ByteBuf, Value};

// ── fauna.auth.handshake (≡ POST /api/v1/auth/token, direct auth) ───────────

/// The exact bytes a `fauna.auth.handshake` request signs (and the nest
/// verifies): `AUTH_HANDSHAKE_V2 ‖ actor_id ‖ timestamp_be ‖ nest_id ‖
/// client_nonce`. **Single source of the signed-message contract** — every
/// signer (`fauna_anon_client::mint_bearer_over_handshake`, the test fixtures'
/// `mint_token_via_handshake`) and the nest verifier
/// (`auth_core::direct_auth_core`) build the message here so they cannot drift
/// apart.
///
/// `nest_id` is the 32-byte identity of the nest the request is addressed to —
/// the channel-binding `nest_actor_id` the client read off this very
/// connection (`fauna_client_core::nest_trust::read_login_binding`; `login.md`
/// § Binding the nest) — and the nest requires it to be its own, so a blob
/// signed for one nest verifies at no other. The unbound form named no nest,
/// which made one login signature a bearer mint at every nest where the actor
/// was registered: a nest the user signed into could relay it.
///
/// The domain tag ([`crate::sig_domain::AUTH_HANDSHAKE_V2`]) is the rule-#8
/// separation of the login context from every other actor-key context.
/// Injective without length prefixes — three fixed-width fields first, the sole
/// variable field (`client_nonce`) as the tail.
///
/// Folding `client_nonce` in uniquifies the otherwise-deterministic Ed25519
/// signature: two clients of the same actor signing within the same millisecond
/// (same `actor_id ‖ timestamp_be`) would otherwise produce a byte-identical
/// signature and the nest's single-use replay guard would reject the second
/// (`auth-handshake finding #1`). A fresh per-request nonce makes the two
/// signatures differ so both mint. The nonce is *inside* the signed message, so
/// stripping or substituting it breaks verification — it adds signature
/// uniqueness, not secrecy, and grants no downgrade.
///
/// Pure byte assembly — intentionally crypto-free so the lean default protocol
/// build stays so (signing/verifying live in the consumers, gated as before).
pub fn handshake_signed_message(
    actor_id: &[u8; 32],
    timestamp_ms: u64,
    nest_id: &[u8; 32],
    client_nonce: &[u8],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(72 + client_nonce.len());
    body.extend_from_slice(actor_id);
    body.extend_from_slice(&timestamp_ms.to_be_bytes());
    body.extend_from_slice(nest_id);
    body.extend_from_slice(client_nonce);
    crate::sig_domain::domain_separated(crate::sig_domain::AUTH_HANDSHAKE_V2, &body)
}

/// The exact bytes a `fauna.auth.verify` (challenge-response) request signs
/// (and the nest verifies): `AUTH_VERIFY_V2 ‖ actor_id ‖ nonce ‖ nest_id` — no
/// timestamp, the server-issued nonce supplies freshness. **Single source of
/// the signed-message contract** — the client signers
/// (`fauna_client_core::auth::build_challenge_verify`, the native silent
/// challenge below) and the nest verifier (`auth_core::verify_core`) build the
/// message here so they cannot drift.
///
/// `nest_id` is the identity of the nest the verify is addressed to, read off
/// this connection before signing (`login.md` § Binding the nest); the nest
/// requires it to be its own. This is the form every app-held bearer is minted
/// over, launch and hourly refresh alike, so its unbound predecessor was a
/// *continuous* relay window rather than a one-shot one. The domain tag
/// ([`crate::sig_domain::AUTH_VERIFY_V2`]) is the rule-#8 separation of the
/// verify context from every other actor-key context; the
/// fixed 32 ‖ 32 ‖ 32 layout is injective by construction.
pub fn challenge_verify_signed_message(
    actor_id: &[u8; 32],
    nonce: &[u8; 32],
    nest_id: &[u8; 32],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(96);
    body.extend_from_slice(actor_id);
    body.extend_from_slice(nonce);
    body.extend_from_slice(nest_id);
    crate::sig_domain::domain_separated(crate::sig_domain::AUTH_VERIFY_V2, &body)
}

/// Direct-auth handshake. Signed message is
/// [`handshake_signed_message`]`(actor_id, timestamp, nest_id, client_nonce)` —
/// `AUTH_HANDSHAKE_V2 ‖ actor_id ‖ timestamp_be ‖ nest_id ‖ client_nonce`; the
/// timestamp must be within ±30 s of server time. Mints a 1-hour bearer with
/// the same side effects as the HTTP twin (account lockout, private-nest
/// reject). The client opens the authenticated `GET /api/v1/ws/{actor_id}` with
/// the returned token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HandshakeRequest {
    /// 64-char hex of the 32-byte Ed25519 public key.
    pub actor_id: String,
    /// Milliseconds since the Unix epoch (the signed `timestamp_be`).
    pub timestamp: u64,
    /// 128-char hex of the 64-byte Ed25519 signature over
    /// [`handshake_signed_message`]`(actor_id, timestamp, nest_id, client_nonce)`.
    pub signature: String,
    /// Fresh per-request 32-byte random nonce the client generates per
    /// connection. **Two purposes from one field:** (1) it is folded into the
    /// client's auth `signature` (see [`handshake_signed_message`]) to uniquify
    /// the otherwise-deterministic Ed25519 signature, so two same-actor clients
    /// signing in the same millisecond don't collide on the replay guard; and
    /// (2) the nest signs `(SPKI_of_the_cert_it_serves ‖ client_nonce)` with
    /// its deployment key and returns the TLS channel-binding proof in
    /// `HandshakeReply.cert_binding` (`docs/goal/architecture/security.md`
    /// § Transport trust, Axis 1).
    pub client_nonce: ByteBuf,
    /// 64-char hex of the receiving nest's identity (the channel-binding
    /// `nest_actor_id` the client read off this connection through
    /// `fauna.auth.nest_handshake`) — bound into `signature` through
    /// [`handshake_signed_message`], and required by the nest to equal its own
    /// identity (`login.md` § Binding the nest). A request naming another nest
    /// is refused with `fauna.auth.invalid_request`.
    pub nest_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The nest→client TLS channel-binding proof — the *wire leg* of
/// `docs/goal/architecture/security.md` § Transport trust, Axis 1. The nest
/// signs the SPKI of the cert **it itself serves** (read from its own live
/// cert resolver, never a client-reported value — the load-bearing subtlety in
/// security.md § Axis 1) concatenated with the client's nonce. The client
/// recomputes `spki_sha256` from the cert it *received* over TLS and verifies
/// `sig` over `(that_spki ‖ client_nonce)` against `nest_actor_id`'s key; the
/// two SPKIs are equal iff no middlebox substituted the cert.
/// The canonical length of every `client_nonce` on the wire — the fresh random
/// value a client folds into an auth signature and the nest folds into its
/// channel-binding proof ([`CertBinding`]). Every producer in the system mints
/// exactly this many bytes (`fauna_anon_client::fresh_nonce`,
/// `fauna_client_core::nest_trust`, `fauna_wasm`), and the field docs on
/// [`HandshakeRequest::client_nonce`], [`DeviceHandshakeRequest::client_nonce`],
/// [`CustodyHandshakeRequest::client_nonce`], [`NestHandshakeRequest::client_nonce`]
/// and [`VerifyRequest::client_nonce`] all say "32 bytes".
///
/// **It is a security bound, not documentation.** The deployment key signs the
/// channel-binding message *untagged* as well as tagged (see [`CertBinding::sig`]),
/// and an untagged deployment-key signature over a 36-byte message is a valid
/// [`fauna_cbor::SignedEnvelope`] signature for whatever value those 36 bytes are
/// the CID of — the KeyBlob exclusion argument of `key-material-hierarchy.md`
/// § Architectural rules #8 rests on no other untagged deployment-key context
/// ever producing one. The nest-side producers therefore *enforce* this length
/// (`auth_handlers::sign_channel_binding`) rather than trusting the field doc, so
/// the signed message length is fixed by construction and can never be 36.
pub const CLIENT_NONCE_LEN: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CertBinding {
    /// 64-char hex of the nest's deployment Ed25519 identity (its `actor_id` —
    /// the `nest_signing_key` public key). The client checks this against its
    /// identity root: DNS `_fauna.{domain}` TXT `self=` for public domains, the
    /// TOFU pin store for LAN/`.local` (security.md § Axis 2).
    pub nest_actor_id: String,
    /// SHA-256 of the `SubjectPublicKeyInfo` of the cert the nest is currently
    /// serving (the public-key fingerprint, **not** the whole-cert fingerprint
    /// — so benign cert re-issuance with the same key does not break it).
    pub spki_sha256: ByteBuf,
    /// **Domain-separated** Ed25519 signature by the deployment key over
    /// [`cert_binding_signed_message`]`(spki_sha256, nonce)` — `b"fauna.cert-binding.v1\0"
    /// ‖ spki_sha256 ‖ nonce` (`fauna_protocol::sig_domain::CERT_BINDING_V1`),
    /// where `nonce` is the request's `client_nonce` on the handshake paths and
    /// `challenge_nonce ‖ client_nonce` (or the bare `challenge_nonce`, for a
    /// client that contributes none) on the verify path. The verifier
    /// reconstructs the same byte sequence from the values it holds. **The
    /// only signature a binding carries**: the untagged `sig` that once rode
    /// beside it as a compat half for pre-tag peers was removed 2026-09-24 by
    /// the compat-remnant sweep (`version-compatibility.md` § Dimension 2, the
    /// fourth in-place ratification), so the deployment key signs no bare
    /// channel-binding bytes anywhere. This is the structural cross-context
    /// guarantee: a signature in any other deployment-key
    /// context can never be reinterpreted as a cert-binding (each carries a
    /// distinct constant prefix — `key-material-hierarchy.md` § Architectural
    /// rules #8). Named `tagged_sig` on the wire because that is the key every
    /// verifier — Rust, the Go bridge, the Python harness — already reads.
    pub tagged_sig: ByteBuf,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The exact bytes a channel binding's [`CertBinding::tagged_sig`] covers:
/// `CERT_BINDING_V1 ‖ spki_sha256 ‖ nonce`. **Single source of the signed-message
/// contract** — the nest's producer (`auth_handlers::sign_channel_binding`), every
/// Rust verifier (`fauna_client_core::nest_trust`), and every test fixture that
/// forges a nest build the message here; the Go bridge
/// (`CertBindingSignedMessage`) and the Python harness
/// (`common.sig_domain.cert_binding_signed_message`) carry byte-identical twins.
pub fn cert_binding_signed_message(spki_sha256: &[u8], nonce: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(spki_sha256.len() + nonce.len());
    msg.extend_from_slice(spki_sha256);
    msg.extend_from_slice(nonce);
    crate::sig_domain::domain_separated(crate::sig_domain::CERT_BINDING_V1, &msg)
}

impl CertBinding {
    /// Sign a channel binding over `spki_sha256 ‖ nonce` with the deployment
    /// key — the one producer behind the nest's handlers and every fixture
    /// that stands in for a nest. Length policy is the caller's: the nest
    /// refuses a non-canonical nonce *before* reaching this (rule #8 guard 1),
    /// and a test that wants a non-canonical shape builds it deliberately.
    pub fn sign(signing_key: &ed25519_dalek::SigningKey, spki_sha256: &[u8], nonce: &[u8]) -> Self {
        use ed25519_dalek::Signer;
        let tagged = signing_key.sign(&cert_binding_signed_message(spki_sha256, nonce));
        Self {
            nest_actor_id: hex::encode(signing_key.verifying_key().to_bytes()),
            spki_sha256: ByteBuf::from(spki_sha256.to_vec()),
            tagged_sig: ByteBuf::from(tagged.to_bytes().to_vec()),
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HandshakeReply {
    /// Opaque bearer token (the HTTP twin's `{"token": …}`).
    pub token: String,
    /// Short 16-hex token id of the minted session (the `token_store` display
    /// id). The client stores it to name its own session in
    /// `fauna.sessions.{revoke,revoke_all}` (`sessions.rs`).
    pub token_id: String,
    /// Unix seconds at which the token expires — on the **nest's** clock.
    pub expires_at: u64,
    /// Seconds the token lives from the moment of this reply — the client's
    /// scheduling input. A client converts it to its own clock at receipt
    /// ([`deadline_on_own_clock`]) and never compares `expires_at` against a
    /// clock the nest did not read; that comparison is what bounced a client
    /// whose clock ran hours ahead (`login.md` § Token lifetime on the client's
    /// clock). Required: every nest sends it (added 2026-09-21; the older-nest
    /// fallback to `expires_at` retired 2026-09-24 with the compat-remnant
    /// sweep).
    pub expires_in: u64,
    /// The TLS channel-binding proof — present iff the nest both holds a
    /// deployment signing key and is serving a TLS cert it can read the SPKI
    /// of. Absent on a plain-HTTP dev nest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_binding: Option<CertBinding>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Pre-expiry buffer: a cached bearer is treated as spent once it is within
/// this many seconds of its [`HandshakeReply::expires_at`], so an in-flight
/// request never presents a token that lapses mid-flight. **Single source of
/// the token-lifetime contract's *spend* rule** — every bearer cache
/// (`fauna-client`'s three `Ws*Bearer`s, `fauna-nest-http`'s
/// `LaunchMachineBearer`, `fauna-sync-engine`'s `WriteTokenBearer`) and the
/// `fauna-launch-machine` TTL-refresh loop reads it here so they cannot drift
/// apart.
///
/// ⚠ **It is not the whole client half.** A cache can only decline to serve a
/// token it already holds; something must also *mint* the successor. While an
/// app runs that is the app. App-dead it is the sync agent's own renewal loop,
/// whose constant — [`AGENT_RENEW_LEAD_SECS`] — lives beside this one and
/// forms the same inequality with the same TTL.
///
/// Lives beside the reply it bounds, in the one crate all six consumers can
/// reach — `fauna-launch-machine` deliberately has no `fauna-nest-http`
/// dependency, so the `BearerSource` trait's own crate cannot own this.
///
/// ⚠ **This is one half of an INEQUALITY, not a value two sides must match**:
/// it must stay strictly below every TTL a nest mints a bearer with
/// (`auth_core::TOKEN_TTL_SECS`, 3600 s, pinned there; the bulk-byte plane's
/// 600 s). The violated direction is the surprising one — a buffer at or above
/// the TTL makes every freshly minted token *born spent*: the cache never
/// serves one, so every request re-mints, and `fauna-launch-machine`'s refresh
/// loop (which sleeps `expires_at - buffer - now`) stops sleeping at all and
/// spins. Both ends stay perfectly self-consistent while this happens, which is
/// why neither side's tests can see it and the nest side carries a
/// compile-time pin instead.
pub const BEARER_REFRESH_BUFFER_SECS: u64 = 60;

/// The deadline a client schedules a bearer's refresh against, on the
/// **client's own** clock — the single conversion every bearer holder makes at
/// receipt (`login.md` § Token lifetime on the client's clock).
///
/// A mint reply's `expires_at` is read on the nest's clock; every deadline the
/// client derives from it (the TTL-refresh sleep, the cache's spend rule, the
/// own-session-id pruning) is compared against the client's clock. On a device
/// whose clock is hours wrong the two clocks disagree by hours, and so did the
/// schedule: hours ahead saturated the sleep to zero and re-minted in a hot loop,
/// hours behind served a token long dead. So the reply also carries
/// `expires_in` — seconds from the reply — and the client anchors it to the
/// clock it will later compare against: `now_client + expires_in`. Nothing about
/// the client's clock is corrected or reported; the nest's freshness rule is
/// untouched.
///
/// `now_client` is `None` when the client's clock cannot be read; then the
/// nest-absolute `expires_at` is the answer, and the hot-loop direction is
/// bounded by the refresh loop's own floor (`fauna-launch-machine`'s
/// `ttl_refresh_loop`), never by this function. `expires_in` itself is never
/// absent: every nest sends it (the older-nest fallback retired 2026-09-24).
pub fn deadline_on_own_clock(
    now_client_secs: Option<u64>,
    expires_in: u64,
    expires_at: u64,
) -> u64 {
    match now_client_secs {
        Some(now) => now.saturating_add(expires_in),
        None => expires_at,
    }
}

/// The sync agent's app-dead renewal **lead**: its loop mints a successor once
/// the provisioned bearer is within this many seconds of `expires_at`
/// (`bins/fauna-sync-agent/src/renewal.rs`, `sync-agent.md` § Credential
/// model). Sized inside the session TTL with margin for clock skew and one
/// retry round of the loop's own backoff.
///
/// **The token-lifetime contract's third participant, and the only one that
/// mints rather than caches.** [`BEARER_REFRESH_BUFFER_SECS`] decides when a
/// *held* token stops being served; this decides when a *new* one is asked for
/// with no app running. It is owned here for exactly the reason that constant
/// is: the agent is a binary, [`crate`] is the only crate it and `fauna-nest`
/// both reach, and the nest is where the TTL it must stay under lives — so the
/// pin can be a compile-time one (`auth_core`, beside `TOKEN_TTL_SECS`).
///
/// ⚠ **One half of an INEQUALITY with `auth_core::TOKEN_TTL_SECS`** — the
/// session TTL the `fauna.auth.device_handshake` mint uses, *not* the bulk-byte
/// plane's, which this loop never touches. At or above that TTL every freshly
/// minted bearer is born already inside its own renewal lead: the loop re-plans
/// one second after each successful mint, finds itself inside the lead again,
/// and mints forever — an unattended background process calling
/// `fauna.auth.device_handshake` at ~1 Hz on every enrolled machine, with no
/// app running to notice. Both ends stay self-consistent while it happens,
/// which is why the pin, not a test, is the guard.
pub const AGENT_RENEW_LEAD_SECS: u64 = 300;

// ── The holder's own session ids ─────────────────────────────────────────────

/// One session token id a bearer holder minted for itself, plus the deadline
/// that retires it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnSessionId {
    id: String,
    expires_at: u64,
}

/// **The set of session ids a bearer holder minted itself** — the current one
/// plus every earlier own id that has not yet expired
/// (`docs/goal/behavior/devices.md` § The client's own session). Memory only:
/// never persisted, so a relaunched app has forgotten its previous run's ids
/// and that run's last token shows as an unmarked row for up to an hour — a
/// bound the goal doc states rather than hides.
///
/// **Single source of the fold rule**, for the same reason
/// [`BEARER_REFRESH_BUFFER_SECS`] is the single source of the spend rule: three
/// holders keep this set — `fauna_client::token_cache::TokenCache` (and through
/// it all three `Ws*Bearer`s), `fauna_launch_machine::LaunchMachine`,
/// and the web SPA's token cache (its own TypeScript twin, since that cache is
/// not Rust) — and a holder that pruned on its own rule would fold a different
/// set of rows into "this app" than its siblings.
///
/// Why a set rather than the one current id: bearers renew hourly (and on any
/// 401) and the predecessor row outlives the renewal, so with only the current
/// id the app's own previous token paints as an unknown second session and
/// *sign out everywhere else* appears to find and kill a stranger.
///
/// `now` is passed in to every method rather than read here: this type is
/// reachable from wasm, where the native clock path is not, and a caller that
/// cannot read its clock passes `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnSessionIds {
    /// Oldest first; the last entry is the most recently minted, which is what
    /// [`Self::current_at`] answers with.
    entries: Vec<OwnSessionId>,
}

impl OwnSessionIds {
    /// Record a freshly-minted session id. Re-recording an id the set already
    /// holds refreshes its deadline and moves it to the end rather than
    /// duplicating it, so a re-mint that returns the same id cannot make one
    /// session paint as two.
    pub fn record(&mut self, id: impl Into<String>, expires_at: u64) {
        let id = id.into();
        self.entries.retain(|e| e.id != id);
        self.entries.push(OwnSessionId { id, expires_at });
    }

    /// Drop every id whose `expires_at` has passed `now`. A `None` clock read
    /// prunes **nothing**: this set annotates a display, and keeping a dead id
    /// only costs an extra fold candidate the nest will never list, whereas
    /// dropping a live one paints the app's own session as a stranger. (The
    /// opposite direction from [`BEARER_REFRESH_BUFFER_SECS`]'s freshness
    /// check, where an unprovable answer must never be served.)
    pub fn prune(&mut self, now: Option<u64>) {
        if let Some(now) = now {
            self.entries.retain(|e| e.expires_at > now);
        }
    }

    /// Every own id still live at `now`, oldest first — the rows an app folds
    /// into its one "this app" row.
    pub fn ids_at(&self, now: Option<u64>) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| now.is_none_or(|now| e.expires_at > now))
            .map(|e| e.id.clone())
            .collect()
    }

    /// The **current** id — the most recently recorded one still live at `now`.
    /// This is what `keep_token_id` is read from, at call time and never from a
    /// previously painted list: a renewal between paint and press must not name
    /// a dead token (`devices.md` § The client's own session).
    pub fn current_at(&self, now: Option<u64>) -> Option<String> {
        self.entries
            .iter()
            .rev()
            .find(|e| now.is_none_or(|now| e.expires_at > now))
            .map(|e| e.id.clone())
    }

    /// True while the set holds no live id.
    pub fn is_empty_at(&self, now: Option<u64>) -> bool {
        self.current_at(now).is_none()
    }
}

/// **Where a sessions surface reads the app's own session ids** — the consumer
/// seam over whichever bearer holder keeps an [`OwnSessionIds`] set
/// (`docs/goal/ui/sessions.md` § Where logic lives). Beside
/// [`crate::RpcRequester`] in spirit: `fauna_client_account::SessionsClient`
/// is generic over both, so the kind logic and the keep-id read are written
/// once for native and wasm. Native: `fauna_client::NestClient` answers
/// through its bearer source; the web SPA answers from its own token cache.
///
/// Static-dispatch `async fn` for [`crate::RpcRequester`]'s reason — per-impl
/// `Send` inference. Both answers are "cannot say" when empty/`None`, never
/// "no sessions".
#[allow(async_fn_in_trait)]
pub trait OwnSessionSource {
    /// Every own id still live — the rows folded into the one "this app" row.
    async fn own_token_ids(&self) -> Vec<String>;
    /// The current own id, read at call time — `keep_token_id`'s only source.
    async fn current_token_id(&self) -> Option<String>;
}

impl<T: OwnSessionSource + ?Sized> OwnSessionSource for std::sync::Arc<T> {
    async fn own_token_ids(&self) -> Vec<String> {
        (**self).own_token_ids().await
    }
    async fn current_token_id(&self) -> Option<String> {
        (**self).current_token_id().await
    }
}

// ── fauna.auth.device_handshake (renewal-grant bearer mint) ──────────────────

/// The pre-identity kind that mints a session bearer against a stored
/// `RenewBearer`-scoped `DeviceAuthorization` — the sync agent's app-dead
/// renewal path (`docs/goal/architecture/apps/sync-agent.md` § Credential
/// model). Additive 2026-07-19.
pub const DEVICE_HANDSHAKE_KIND: &str = "fauna.auth.device_handshake";

/// The exact bytes a `fauna.auth.device_handshake` request signs (and the nest
/// verifies): `DEVICE_HANDSHAKE_V2 ‖ actor_id ‖ device_key ‖ timestamp_be ‖
/// nest_id ‖ nonce`. **Single source of the signed-message contract** — the
/// agent-side signer (`fauna-anon-client`) and the nest verifier
/// (`auth_core::device_auth_core`) both build the message here so they cannot
/// drift apart.
///
/// `nest_id` is the receiving nest's identity, bound in before the variable
/// tail exactly as [`handshake_signed_message`] binds it for the login
/// handshake (`login.md` § Binding the nest): a grant the app registered on
/// two linked nests would otherwise make one signature a bearer at either.
/// The nonce is mandatory: it uniquifies the deterministic Ed25519 signature
/// for the replay guard and doubles as the TLS channel-binding nonce, both
/// exactly as in [`handshake_signed_message`].
pub fn device_handshake_signed_message(
    actor_id: &[u8; 32],
    device_key: &[u8; 32],
    timestamp_ms: u64,
    nest_id: &[u8; 32],
    nonce: &[u8],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(104 + nonce.len());
    body.extend_from_slice(actor_id);
    body.extend_from_slice(device_key);
    body.extend_from_slice(&timestamp_ms.to_be_bytes());
    body.extend_from_slice(nest_id);
    body.extend_from_slice(nonce);
    crate::sig_domain::domain_separated(crate::sig_domain::DEVICE_HANDSHAKE_V2, &body)
}

/// The exact bytes a `fauna.sync.device_grant.revoke` proof-of-possession signs
/// (and the nest verifies): `DEVICE_GRANT_REVOKE_V1 ‖ actor_id ‖ device_key ‖
/// timestamp_be ‖ nonce` — the **self arm** of the grant revoke
/// (`sync-agent.md` § Credential model → the RULED 2026-08-15 block,
/// decision 2). Single source of the contract, exactly as
/// [`device_handshake_signed_message`] is for the mint: the agent-side signer
/// (`fauna-client-sync`) and the nest verifier (`sync_handlers`) both build it
/// here.
///
/// The field layout is deliberately identical to the mint's, and the **domain
/// tag is the only thing separating them** — which is why
/// [`crate::sig_domain::DEVICE_GRANT_REVOKE_V1`] is its own tag rather than a
/// reuse: same key, same fields, opposite meanings.
pub fn device_grant_revoke_signed_message(
    actor_id: &[u8; 32],
    device_key: &[u8; 32],
    timestamp_ms: u64,
    nonce: &[u8],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(72 + nonce.len());
    body.extend_from_slice(actor_id);
    body.extend_from_slice(device_key);
    body.extend_from_slice(&timestamp_ms.to_be_bytes());
    body.extend_from_slice(nonce);
    crate::sig_domain::domain_separated(crate::sig_domain::DEVICE_GRANT_REVOKE_V1, &body)
}

/// The exact bytes a `fauna.sync.devices.p2p_participation.set` self-arm
/// proof of possession signs (and the nest verifies):
/// `DEVICE_P2P_PARTICIPATION_V1 ‖ actor_id ‖ device_key ‖ device_id ‖
/// participating ‖ timestamp_be ‖ nonce`, `participating` as one byte (`1`
/// on, `0` off) — the device's own report of whether it runs its peer
/// listeners (`docs/goal/behavior/p2p.md` § Per-device participation).
/// Single source of the contract, exactly as its three siblings above: the
/// pump-side signer (`fauna-client-sync`) and the nest verifier
/// (`sync_handlers`) both build it here.
///
/// Both the row and the verdict are inside the signature: a report names
/// one device and one direction, so a captured `on` can neither be replayed
/// as `off` (a denial of participation any eavesdropper could mount) nor
/// aimed at another row.
pub fn device_p2p_participation_signed_message(
    actor_id: &[u8; 32],
    device_key: &[u8; 32],
    device_id: &[u8; 32],
    participating: bool,
    timestamp_ms: u64,
    nonce: &[u8],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(105 + nonce.len());
    body.extend_from_slice(actor_id);
    body.extend_from_slice(device_key);
    body.extend_from_slice(device_id);
    body.push(u8::from(participating));
    body.extend_from_slice(&timestamp_ms.to_be_bytes());
    body.extend_from_slice(nonce);
    crate::sig_domain::domain_separated(crate::sig_domain::DEVICE_P2P_PARTICIPATION_V1, &body)
}

/// Renewal-grant handshake. Signed message is
/// [`device_handshake_signed_message`]; the timestamp must be within ±30 s of
/// server time, and the signing key is the **renewal device key** (never the
/// identity keypair) whose public half a stored `RenewBearer`-scoped
/// `DeviceAuthorization` authorizes. Mints an ordinary 1-hour session bearer
/// with the direct-auth side effects (active/lockout checks, replay guard).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceHandshakeRequest {
    /// 64-char hex of the owner's 32-byte Ed25519 public key (the actor the
    /// minted bearer acts as).
    pub actor_id: String,
    /// 64-char hex of the renewal device public key the stored grant
    /// authorizes.
    pub device_key: String,
    /// Milliseconds since the Unix epoch (the signed `timestamp_be`).
    pub timestamp: u64,
    /// 128-char hex of the 64-byte Ed25519 signature by the renewal device key
    /// over [`device_handshake_signed_message`].
    pub signature: String,
    /// Fresh per-request 32-byte random nonce — **mandatory** (see
    /// [`device_handshake_signed_message`]). Folded into the signature and used
    /// for the reply's TLS channel binding, as in
    /// [`HandshakeRequest::client_nonce`].
    pub client_nonce: ByteBuf,
    /// 64-char hex of the receiving nest's identity, bound into `signature`
    /// through [`device_handshake_signed_message`] — same contract as
    /// [`HandshakeRequest::nest_id`].
    pub nest_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// The reply is [`HandshakeReply`] — same token/token_id/expires_at/cert_binding
// shape as the direct-auth mint; a distinct reply type would restate it field
// for field.

// ── fauna.auth.custody_handshake (custody-session bearer mint) ───────────────

/// The pre-identity kind that mints a CUSTODIAN session bearer against a
/// custody-grant witness + a live capability row (W8.6 (account-data-plane.md § Workstreams) —
/// `account-data-plane.md` § Replica posture → *The custody grant +
/// ceremony*: "a custody handshake kind on the anonymous surface — witness +
/// a server-challenge proof-of-possession signed by `custodian_key`").
/// Additive 2026-08-16.
///
/// The minted bearer's actor is the **custodian key itself** — never the
/// owner: authorization is re-derived from the live custody row on every
/// request (the nest door's per-request re-check), and a custodian-actor
/// bearer fails every other permission gate closed by construction.
pub const CUSTODY_HANDSHAKE_KIND: &str = "fauna.auth.custody_handshake";

/// The exact bytes a `fauna.auth.custody_handshake` request signs (and the
/// nest verifies): `CUSTODY_HANDSHAKE_V2 ‖ owner_actor_id ‖ custodian_key ‖
/// timestamp_be ‖ nest_id ‖ nonce` — the [`device_handshake_signed_message`]
/// layout under its own domain tag (same key family, overlapping fields,
/// different authority — the standing tag-per-context rule), the receiving
/// nest bound in the same way (`login.md` § Binding the nest). Single source
/// of the signed-message contract for the client-side signer and the nest
/// verifier.
pub fn custody_handshake_signed_message(
    owner_actor_id: &[u8; 32],
    custodian_key: &[u8; 32],
    timestamp_ms: u64,
    nest_id: &[u8; 32],
    nonce: &[u8],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(104 + nonce.len());
    body.extend_from_slice(owner_actor_id);
    body.extend_from_slice(custodian_key);
    body.extend_from_slice(&timestamp_ms.to_be_bytes());
    body.extend_from_slice(nest_id);
    body.extend_from_slice(nonce);
    crate::sig_domain::domain_separated(crate::sig_domain::CUSTODY_HANDSHAKE_V2, &body)
}

/// Custody-session handshake. Signed message is
/// [`custody_handshake_signed_message`]; the timestamp must be within ±30 s
/// of server time, the signing key is the CUSTODIAN's device-principal key
/// (whose public half the witness names), and the witness must verify under
/// the named owner AND a live custody capability row must exist for
/// `(owner, witness.grant_id)` with `holder == custodian_key`. Mints an
/// ordinary session bearer whose actor is `custodian_key`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CustodyHandshakeRequest {
    /// 64-char hex of the custodied OWNER's 32-byte actor id.
    pub owner_actor_id: String,
    /// 64-char hex of the custodian's device-principal public key (= its
    /// peer-plane NodeId) — the key the PoP signature proves and the witness
    /// must name.
    pub custodian_key: String,
    /// Milliseconds since the Unix epoch (the signed `timestamp_be`).
    pub timestamp: u64,
    /// 128-char hex of the 64-byte Ed25519 signature by `custodian_key`
    /// over [`custody_handshake_signed_message`].
    pub signature: String,
    /// Fresh per-request 32-byte random nonce — mandatory (replay-guard
    /// uniquifier + TLS channel-binding nonce, the device-handshake shape).
    pub client_nonce: ByteBuf,
    /// The custody-grant admission witness (`fauna_core::custody_grant`),
    /// carried inline — self-contained carriage, the admission seam's rule.
    pub witness: fauna_core::encoding::EmbedAsBytes,
    /// 64-char hex of the receiving nest's identity, bound into `signature`
    /// through [`custody_handshake_signed_message`] — same contract as
    /// [`HandshakeRequest::nest_id`].
    pub nest_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// The reply is [`HandshakeReply`], like the device handshake's.

// ── fauna.auth.nest_handshake (pre-identity nest-identity handshake) ─────────

/// The pre-identity nest-identity handshake kind — the client asks the nest to
/// prove *its* identity over the live TLS channel **before any actor exists**
/// (unlike `fauna.auth.handshake`, which mints a bearer for a registered actor
/// and only carries the binding as a side effect). The first-contact trust leg
/// of the client-provisioned-box flow: the onboarding machine runs it as the
/// opening step of every pre-claim connection and graduates the returned
/// binding against the deployment seed it injected at provision
/// (`docs/goal/architecture/security.md` § Transport trust, Axis 2 —
/// client-provisioned row; design tracked internally).
pub const NEST_HANDSHAKE_KIND: &str = "fauna.auth.nest_handshake";

/// Ask the nest to sign a TLS channel binding over a fresh client nonce —
/// no actor, no signature, no side effects (pure read; replay-safe because the
/// client-chosen nonce uniquifies each signature and nothing is mutated).
///
/// Since 2026-09-23 this is also **the opening leg of every bearer mint**: the
/// identity it proves is what every login signature binds
/// (`HandshakeRequest::nest_id` and its three siblings; `login.md` § Binding the
/// nest — `fauna_client_core::nest_trust::read_login_binding`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NestHandshakeRequest {
    /// Fresh per-request 32-byte random nonce the client generates. The nest
    /// signs `(SPKI_of_the_cert_it_serves ‖ client_nonce)` — the same Axis-1
    /// channel-binding message as `HandshakeRequest.client_nonce`, so the
    /// client-side verifier (`verify_cert_binding`) is shared unchanged.
    pub client_nonce: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NestHandshakeReply {
    /// The TLS channel-binding proof — the same shape `HandshakeReply` carries.
    /// `None` when the nest cannot produce one (a plain-HTTP dev nest with no
    /// served-cert SPKI, or a keyless nest); the client then falls back to its
    /// non-provisioned trust path (DNS `self=` / TOFU).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_binding: Option<CertBinding>,
    /// Capability adverts ride here as additive fields when a future
    /// versioned ceremony needs one (the mechanism `version-compatibility.md`
    /// § Dimension 3 keeps). The `nat_mode_v2` advert that once gated the
    /// nest-bound NAT-mode commit was retired 2026-09-24 with the V1 form it
    /// selected against (the compat-remnant sweep): V2 is the only form, so
    /// nothing is advertised today.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.auth.challenge (≡ POST /api/v1/auth/challenge, nonce issuance) ─────

/// Request a fresh challenge nonce for `actor_id`. No security side effects.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChallengeRequest {
    /// 64-char hex of the 32-byte Ed25519 public key.
    pub actor_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChallengeReply {
    /// 64-char hex of the 32-byte server-issued nonce.
    pub nonce: String,
    /// Seconds until the nonce expires (5-minute TTL).
    pub expires_in: u64,
    /// Unix seconds at which the nonce expires.
    pub expires_at: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.auth.verify (≡ POST /api/v1/auth/verify, nonce-signed token) ───────

/// Verify a signed challenge nonce and mint a bearer + cached metadata. Signed
/// message is [`challenge_verify_signed_message`]`(actor_id, nonce, nest_id)` —
/// `AUTH_VERIFY_V2 ‖ actor_id ‖ nonce ‖ nest_id` (**no timestamp** — the nonce
/// supplies freshness). Unregistered actors are rejected with
/// `fauna.auth.not_registered` (the HTTP twin's 404), the launch flow's
/// drop-into-onboarding signal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerifyRequest {
    /// 64-char hex of the 32-byte Ed25519 public key.
    pub actor_id: String,
    /// 64-char hex of the 32-byte nonce from `fauna.auth.challenge`.
    pub nonce: String,
    /// 128-char hex of the 64-byte Ed25519 signature over
    /// [`challenge_verify_signed_message`]`(actor_id, nonce, nest_id)`.
    pub signature: String,
    /// 64-char hex of the receiving nest's identity (read off this connection
    /// through `fauna.auth.nest_handshake` before signing), bound into
    /// `signature` and required by the nest to equal its own identity
    /// (`login.md` § Binding the nest) — same contract as
    /// [`HandshakeRequest::nest_id`].
    pub nest_id: String,
    /// Fresh per-request 32-byte random nonce the **client** generates, folded
    /// into the TLS channel-binding proof only (it is **not** part of the auth
    /// `signature` above — the server `nonce` already supplies auth freshness).
    /// When present, the nest signs `(served_SPKI ‖ challenge_nonce ‖
    /// client_nonce)` in `VerifyReply.cert_binding`, so the web pin's binding is
    /// bound to a *client*-chosen value — symmetric with the handshake path's
    /// `HandshakeRequest.client_nonce` (`docs/goal/architecture/security.md`
    /// § Transport trust). This closes the NT-1 offline-harvest replay: the
    /// verify path's only freshness lever was otherwise the server-chosen
    /// `challenge_nonce`, which an attacker can replay (security review
    /// finding NT-1; tracked internally).
    /// A client that will not consult the reply's binding sends none — the
    /// web SPA, the Go bridge and the Python harness pin the nest through the
    /// opening `fauna.auth.nest_handshake` read and the bound login signature
    /// instead (`login.md` § Binding the nest) — and the binding then signs
    /// `(served_SPKI ‖ challenge_nonce)`. The verifier checks exactly the
    /// message shape the request sent: a caller that folded a nonce verifies
    /// the 3-part message and nothing else (the challenge-only retry that once
    /// served a nest that ignored the nonce fold was retired 2026-09-24 by the compat-remnant
    /// sweep).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_nonce: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerifyReply {
    /// Opaque bearer token.
    pub token: String,
    /// Short 16-hex token id of the minted session (the `token_store` display
    /// id). The client stores it to name its own session in
    /// `fauna.sessions.{revoke,revoke_all}` (`sessions.rs`).
    pub token_id: String,
    /// The actor's handle (empty string if unset), for "Welcome back, @handle".
    pub handle: String,
    /// The nest's handle domain.
    pub domain: String,
    /// The actor's tier name.
    pub tier: String,
    /// Unix seconds at which the token expires — on the **nest's** clock.
    pub expires_at: u64,
    /// Seconds the token lives from the moment of this reply — the client's
    /// scheduling input, same contract as [`HandshakeReply::expires_in`] (see
    /// [`deadline_on_own_clock`]). Required: every nest sends it.
    pub expires_in: u64,
    /// The TLS channel-binding proof — present iff the nest both holds a
    /// deployment signing key and is serving a TLS cert it can read the SPKI
    /// of. Absent on a plain-HTTP dev nest. Carries the same Axis-1 proof the
    /// direct `fauna.auth.handshake` path does (`HandshakeReply::cert_binding`),
    /// so a native client that authenticates via the challenge/verify ceremony
    /// can still possession-verify and pin the nest's identity
    /// (`docs/goal/architecture/security.md` § Transport trust, Axis 1; the nest
    /// signs `spki_sha256 ‖ challenge_nonce ‖ client_nonce` when the request
    /// carried a [`VerifyRequest::client_nonce`], else `spki_sha256 ‖
    /// challenge_nonce` — the NT-1 hardening).
    ///
    /// **`Box`ed** (unlike `HandshakeReply::cert_binding`, which is unboxed) only
    /// to keep [`SilentChallengeOutcome::Success`]`(VerifyReply)` under
    /// `clippy::large_enum_variant`: `VerifyReply` is the one of the two replies
    /// that rides a multi-variant outcome enum. The wire is identical — `Box<T>`
    /// (de)serializes transparently as `T`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_binding: Option<Box<CertBinding>>,
    /// Forward-compat catch-all. (The `storage_mode_pending` flag that once
    /// rode here — always `false` since the storage-mode axis retired, kept
    /// only to steer a client past a wizard step that no longer existed — left the wire 2026-09-24 with the compat-remnant sweep; a
    /// peer that still sends it lands here and is ignored.)
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Catalog-aligned fixture default (an authenticated reply for an ordinary,
/// fully-set-up nest), so fixtures grow new fields via struct-update syntax
/// (`..Default::default()`) instead of hand-listing every field — two branches
/// independently growing this wire type then merge cleanly.
impl Default for VerifyReply {
    fn default() -> Self {
        Self {
            token: String::new(),
            token_id: String::new(),
            handle: String::new(),
            domain: String::new(),
            tier: String::new(),
            expires_at: 0,
            expires_in: 0,
            cert_binding: None,
            extra: BTreeMap::new(),
        }
    }
}

// ── Silent-challenge ceremony (fauna.auth.challenge + fauna.auth.verify) ─────
//
// The generic two-round-trip challenge/verify ceremony, written **once** here
// next to the wire types it drives and the [`RpcRequester`](crate::RpcRequester)
// trait it is generic over (priority #2). It lives in `fauna-protocol` rather
// than in a client crate because **two** otherwise-unrelated state machines need
// it and must not depend on each other: `fauna-launch-machine` (the app-launch
// silent sign-in) and `fauna-onboarding-machine` (the wizard's handle-check
// probe, which asks "is this keypair registered on this nest, under what
// handle"). Both already depend on `fauna-protocol`; routing the ceremony
// through a client crate would form the documented `fauna-client →
// fauna-nest-http → fauna-launch-machine` Cargo cycle. The signing deps
// (`ed25519-dalek`, `hex`) are gated behind the off-by-default `auth-ceremony`
// feature, mirroring the `tls-spki` channel-binding helper, so the lean default
// protocol build stays crypto-free.
//
// The `fauna.auth.handshake` direct-auth ceremony is **not** here: its one
// Rust client home is `fauna_anon_client::mint_bearer_over_handshake`, and
// `login.md` § When to use which assigns no app-held bearer to it — every app
// mint, launch and refresh alike, is this ceremony (`challenge_verify` below;
// `fauna_anon_client::mint_bearer_over_silent_challenge` for a held bearer).

/// Outcome of a silent-challenge attempt. Maps 1:1 to the failure-mode
/// fallbacks in `docs/goal/behavior/onboarding.md` lines 248–252.
///
/// Reachability failures (connect refused, disconnect, timeout, server error,
/// captive portal, decommissioned nest) collapse into a single `Transient`
/// bucket. The target doc's argument: client-side classification of transient
/// vs terminal is fundamentally unreliable, and a false-terminal
/// misclassification — alarming the user and dropping them to handle_entry when
/// retry would have worked — is strictly worse than offering Retry every time.
#[derive(Debug, Clone)]
pub enum SilentChallengeOutcome {
    /// Both round-trips succeeded; the actor is registered on this nest. The
    /// [`VerifyReply`] carries the bearer + the actor's `handle`/`domain`/`tier`.
    Success(VerifyReply),
    /// `fauna.auth.verify` returned `fauna.auth.not_registered` — the actor
    /// isn't registered on this nest. Launch flow drops to wizard at
    /// `invite_request`; the wizard's handle-check reads it as `Unregistered`.
    NotRegistered,
    /// Any reachability failure: server error, timeout, malformed response,
    /// disconnect, connect refused, or an expired/consumed nonce (retryable —
    /// a retry gets a fresh nonce).
    Transient { error: String },
    /// `fauna.nest.outdated` — the nest booted a degraded "needs-update" mode and
    /// can't serve until it is updated (version-compatibility.md Dim 4). The
    /// secret is fine; **not** retryable. `message` is the localized actionable
    /// banner. Distinguished from [`Self::Transient`] so the launch machine
    /// surfaces an update prompt rather than spinning a retry loop. Can arrive at
    /// either ceremony round-trip (the degraded nest rejects every RPC).
    NeedsUpdate { message: String },
    /// The supplied secret isn't a 32-byte Ed25519 key. Terminal.
    SecretInvalid { error: String },
    /// `fauna.auth.superseded` — this identity was succeeded, and the account
    /// now belongs to `new_actor_id_hex`
    /// (`docs/goal/behavior/identity-succession.md` § Propagation → *Own device
    /// fleet*). **Terminal, and terminal for a reason no retry can change**: the
    /// old key still produces valid signatures forever, so re-signing only
    /// re-earns the refusal. Distinguished from [`Self::Transient`] for exactly
    /// the reason [`Self::NeedsUpdate`] is — and from [`Self::SecretInvalid`]
    /// because the secret is *fine*, it simply no longer owns this account.
    ///
    /// The successor is **claimed, not proven**: it is whatever the refusal
    /// named. A consumer verifies it against the registration chain
    /// (`fauna_client_recovery::resolve_successor`) before presenting it as
    /// fact — the nest is enforcer and distributor, never authorizer.
    Superseded { new_actor_id_hex: String },
    /// `fauna.auth.account_locked` — the account is locked out until
    /// `locked_until_secs` (Unix seconds; `login.md` § Silent Challenge,
    /// enforcement ruled 2026-09-25). **Terminal until then**: no retry can
    /// clear it before its time, so it is distinguished from [`Self::Transient`]
    /// (which would spin a retry loop) and, like [`Self::Superseded`], the
    /// secret is fine. An older client that predates this arm sees an unknown
    /// code and lands `Transient` (retry surface) — additive-compatible.
    Locked { locked_until_secs: u64 },
    /// The nest's **pinned deployment identity** changed (or the nest could no
    /// longer prove the identity a pin exists for — the downgrade case,
    /// `seen_hex: None`). The SSH `known_hosts` model
    /// (`docs/goal/architecture/security.md` § Transport trust):
    /// auto-entry is BLOCKED and the user must explicitly re-trust (forget the
    /// pin and re-TOFU) — never a silent re-pin, never a retry loop.
    ///
    /// Produced by the trust layer, not the ceremony itself: natively the
    /// channel-binding check at connect raises `IdentityError::PinChanged`
    /// (mapped by `fauna-launch-machine`'s connect wrapper); on wasm the
    /// possession-verify + pin compare after the verify reply raises it
    /// (`fauna_client_core::nest_trust::run_pinned_silent_challenge`); and
    /// the wizard's pre-claim seam (`fauna-onboarding-machine`'s
    /// `WsNestApi::silent_challenge`) maps a connect-stage graduation failure
    /// here — a pin-related failure, or ANY failure while a first-contact
    /// root is held (security.md § Pre-claim surfacing). Wizard consumers
    /// treat it as terminal with NO re-trust affordance: with a held root the
    /// remedy is re-provisioning, and `pinned_hex` is then the held root, not
    /// a TOFU pin. `host` is the pin key (nest URL/origin); fingerprints are
    /// hex `nest_actor_id`s for the warning surface.
    IdentityChanged {
        host: String,
        pinned_hex: String,
        seen_hex: Option<String>,
        /// Rotation-chain **fork evidence** (`box-recovery.md` § Client
        /// acceptance): the box's served rotation history contradicts the one
        /// this client accepted earlier. The warning still names both heads,
        /// but there is **no re-trust on this surface** — the launch machine
        /// refuses `trust_nest_identity` for it. `false` is the ordinary
        /// changed/withdrawn case (explicit re-trust available, unchanged).
        fork: bool,
    },
}

/// `fauna.auth.challenge` — nonce issuance (≡ `POST /auth/challenge`).
#[cfg(feature = "auth-ceremony")]
const CHALLENGE_KIND: &str = "fauna.auth.challenge";
/// `fauna.auth.verify` — nonce-signed token issuance (≡ `POST /auth/verify`).
#[cfg(feature = "auth-ceremony")]
const VERIFY_KIND: &str = "fauna.auth.verify";

/// Drive the two-round-trip challenge/verify ceremony over an already-connected
/// anonymous requester, using `secret` (a 32-byte Ed25519 key) to sign. Signs
/// [`challenge_verify_signed_message`]`(actor_id, nonce, nest_id)` (**no
/// timestamp** — the server-issued nonce is freshness). `nest_id` is the
/// identity of the nest at the far end of `client`, which the caller read
/// first through `fauna_client_core::nest_trust::read_login_binding` — the one
/// reader every login signer shares (`login.md` § Binding the nest).
#[cfg(feature = "auth-ceremony")]
pub async fn run_silent_challenge<R>(
    client: &R,
    secret: &[u8],
    nest_id: &[u8; 32],
) -> SilentChallengeOutcome
where
    R: crate::RpcRequester,
    R::Error: crate::RpcErrorClass,
{
    run_silent_challenge_with_nonce(client, secret, None, nest_id)
        .await
        .0
}

/// [`run_silent_challenge`] with the NT-1 verify-path binding levers exposed:
/// `client_nonce` is folded into the nest's `cert_binding` proof when given
/// (`VerifyRequest.client_nonce`), and the server-issued challenge nonce is
/// returned alongside the outcome — the two inputs a caller needs to
/// possession-verify `VerifyReply.cert_binding` afterwards
/// (`fauna_client_core::nest_trust::run_pinned_silent_challenge` is that
/// caller; the native path never passes a nonce — its identity pin rides the
/// connect-time channel binding instead, see the `client_nonce: None` note
/// below).
#[cfg(feature = "auth-ceremony")]
pub async fn run_silent_challenge_with_nonce<R>(
    client: &R,
    secret: &[u8],
    client_nonce: Option<&[u8; 32]>,
    nest_id: &[u8; 32],
) -> (SilentChallengeOutcome, Option<[u8; 32]>)
where
    R: crate::RpcRequester,
    R::Error: crate::RpcErrorClass,
{
    use crate::RpcErrorClass;
    use ed25519_dalek::SigningKey;

    // 1. Validate the secret bytes.
    let secret_arr: [u8; 32] = match <[u8; 32]>::try_from(secret) {
        Ok(s) => s,
        Err(_) => {
            return (
                SilentChallengeOutcome::SecretInvalid {
                    error: format!("expected 32-byte secret, got {} bytes", secret.len()),
                },
                None,
            );
        }
    };
    let signing = SigningKey::from_bytes(&secret_arr);

    // 2–4. The ceremony itself, then classify where it stopped. The native
    // launch path never passes a `client_nonce`: it pins the nest identity via
    // the *connect-time* channel binding (full SPKI compare), so it never
    // possession-verifies the reply's `cert_binding`. Callers that do
    // possession-verify it fold a fresh client nonce in (the NT-1 hardening):
    // `fauna-wasm`'s `challenge_verify_inner`,
    // `nest_trust::run_pinned_silent_challenge`, and the app-held bearer mint
    // `fauna_anon_client::mint_bearer_over_silent_challenge`.
    match challenge_verify(client, &signing, client_nonce, nest_id).await {
        Ok((reply, nonce)) => (SilentChallengeOutcome::Success(reply), Some(nonce)),
        Err(ChallengeVerifyFailure::Challenge(e)) => (silent_challenge_error(&e), None),
        Err(ChallengeVerifyFailure::MalformedNonce) => (
            SilentChallengeOutcome::Transient {
                error: "invalid nonce hex from server".into(),
            },
            None,
        ),
        Err(ChallengeVerifyFailure::Verify {
            error,
            challenge_nonce,
        }) => {
            let outcome = match error.as_rpc_error() {
                // The actor isn't registered → drop into onboarding. Every other
                // rejection (incl. an expired/consumed nonce) and every transport
                // fault is retryable (a retry gets a fresh nonce) — except
                // `fauna.nest.outdated`, which `silent_challenge_error` routes to
                // the actionable update prompt.
                Some(rpc) if rpc.code == "fauna.auth.not_registered" => {
                    SilentChallengeOutcome::NotRegistered
                }
                _ => silent_challenge_error(&error),
            };
            (outcome, Some(challenge_nonce))
        }
    }
}

/// Where a raw [`challenge_verify`] ceremony stopped — the transport's own
/// error kept **verbatim**, so a caller that needs the refusal itself (a
/// superseded refusal's successor, the exact `fauna.auth.*` code) still has it.
/// [`run_silent_challenge_with_nonce`] folds this into the launch path's
/// [`SilentChallengeOutcome`] taxonomy; the app-held bearer mint
/// (`fauna_anon_client::mint_bearer_over_silent_challenge`) maps it straight
/// onto its transport error instead.
#[cfg(feature = "auth-ceremony")]
#[derive(Debug)]
pub enum ChallengeVerifyFailure<E> {
    /// `fauna.auth.challenge` failed (a transport fault or a nest refusal).
    Challenge(E),
    /// The nest's challenge nonce was not 32 bytes of hex.
    MalformedNonce,
    /// `fauna.auth.verify` failed; the challenge nonce it was signed over rides
    /// along for a caller that still wants it.
    Verify { error: E, challenge_nonce: [u8; 32] },
}

/// The two-round-trip challenge/verify ceremony, **unclassified**: request a
/// nonce for `signing`'s actor (`fauna.auth.challenge`), sign
/// [`challenge_verify_signed_message`]`(actor_id, nonce, nest_id)` (**no
/// timestamp** — the server-issued nonce is freshness, which is what makes
/// this the ceremony for a device whose clock may be wrong; `login.md` § When
/// to use which), and verify it (`fauna.auth.verify`), folding `client_nonce`
/// into the nest's `cert_binding` proof when given. Returns the reply and the
/// challenge nonce — the two inputs a caller needs to possession-verify the
/// binding afterwards.
///
/// `nest_id` — the receiving nest's identity, read off this connection by the
/// caller (`fauna_client_core::nest_trust::read_login_binding`) — is bound into
/// the signature and sent as `VerifyRequest::nest_id`, so a nest the user signs
/// into cannot relay the blob and mint the user a bearer elsewhere (`login.md`
/// § Binding the nest).
///
/// The one copy of the ceremony's wire shape: [`run_silent_challenge_with_nonce`]
/// classifies on top of it, and the app-held bearer mint
/// (`fauna_anon_client::mint_bearer_over_silent_challenge`) consumes it raw.
#[cfg(feature = "auth-ceremony")]
pub async fn challenge_verify<R>(
    client: &R,
    signing: &ed25519_dalek::SigningKey,
    client_nonce: Option<&[u8; 32]>,
    nest_id: &[u8; 32],
) -> Result<(VerifyReply, [u8; 32]), ChallengeVerifyFailure<R::Error>>
where
    R: crate::RpcRequester,
{
    use ed25519_dalek::Signer;

    let actor_id = signing.verifying_key().to_bytes();
    let actor_id_hex = hex::encode(actor_id);

    let challenge: ChallengeReply = client
        .request(
            CHALLENGE_KIND,
            ChallengeRequest {
                actor_id: actor_id_hex.clone(),
                extra: Default::default(),
            },
        )
        .await
        .map_err(ChallengeVerifyFailure::Challenge)?;

    let nonce_bytes: [u8; 32] = fauna_core::hex32::decode(&challenge.nonce)
        .map_err(|_| ChallengeVerifyFailure::MalformedNonce)?;

    // Sign the tagged, nest-bound verify message (actor_id ‖ nonce ‖ nest_id;
    // no timestamp — server's nonce is freshness) through the single-source
    // builder.
    let msg = challenge_verify_signed_message(&actor_id, &nonce_bytes, nest_id);
    let signature = signing.sign(&msg);

    let reply = client
        .request::<_, VerifyReply>(
            VERIFY_KIND,
            VerifyRequest {
                actor_id: actor_id_hex,
                nonce: challenge.nonce,
                signature: hex::encode(signature.to_bytes()),
                client_nonce: client_nonce.map(|n| crate::ByteBuf::from(n.to_vec())),
                nest_id: hex::encode(nest_id),
                extra: Default::default(),
            },
        )
        .await
        .map_err(|error| ChallengeVerifyFailure::Verify {
            error,
            challenge_nonce: nonce_bytes,
        })?;
    Ok((reply, nonce_bytes))
}

/// Shared error classifier for the silent-challenge round-trips. A degraded nest
/// answers `fauna.nest.outdated` to **every** pre-identity kind — the opening
/// `fauna.auth.nest_handshake` identity read as much as
/// `fauna.auth.{challenge,verify}` — so every call site funnels through here:
/// map that code to the actionable [`SilentChallengeOutcome::NeedsUpdate`]
/// (update prompt, not retry — version-compatibility.md Dim 4); everything
/// else stays a retryable [`SilentChallengeOutcome::Transient`] (the doc's
/// "offer Retry every time" stance for reachability faults). `pub` because the
/// identity read runs *outside* this crate (its verifier lives in
/// `fauna-client-core`), and its refusal must classify identically.
#[cfg(feature = "auth-ceremony")]
pub fn silent_challenge_error<E>(e: &E) -> SilentChallengeOutcome
where
    E: crate::RpcErrorClass + std::fmt::Display,
{
    match e.as_rpc_error() {
        Some(rpc) if rpc.code == crate::RpcError::CODE_NEST_OUTDATED => {
            SilentChallengeOutcome::NeedsUpdate {
                message: rpc.localized().to_string(),
            }
        }
        // The identity was succeeded. Read the successor off the refusal rather
        // than the code alone — `superseded_by` returns `None` for every other
        // code, so a malformed refusal that names no successor falls through to
        // the retryable bucket rather than inventing an empty successor.
        Some(rpc) => match (rpc.superseded_by(), rpc.locked_until_secs()) {
            (Some(id), _) => SilentChallengeOutcome::Superseded {
                new_actor_id_hex: fauna_core::hex32::encode(&id),
            },
            // Same fall-through discipline: a lock refusal with no readable
            // `locked_until` stays retryable rather than inventing a time.
            (None, Some(locked_until_secs)) => SilentChallengeOutcome::Locked { locked_until_secs },
            (None, None) => SilentChallengeOutcome::Transient {
                error: e.to_string(),
            },
        },
        _ => SilentChallengeOutcome::Transient {
            error: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};
    // ── OwnSessionIds — the fold rule every bearer holder shares ────────────

    #[test]
    fn two_successive_mints_keep_both_ids_until_the_first_expires() {
        let mut own = OwnSessionIds::default();
        own.record("aaaaaaaaaaaaaaaa", 1_000_000_100);
        own.record("bbbbbbbbbbbbbbbb", 1_000_003_700);
        // Both live: the renewal's predecessor row outlives the renewal, and
        // both must fold into the one "this app" row.
        assert_eq!(
            own.ids_at(Some(1_000_000_000)),
            vec![
                "aaaaaaaaaaaaaaaa".to_string(),
                "bbbbbbbbbbbbbbbb".to_string()
            ]
        );
        // Past the first's deadline, only the successor remains.
        assert_eq!(
            own.ids_at(Some(1_000_000_200)),
            vec!["bbbbbbbbbbbbbbbb".to_string()]
        );
    }

    #[test]
    fn current_is_the_newest_recorded_id() {
        let mut own = OwnSessionIds::default();
        own.record("aaaaaaaaaaaaaaaa", 1_000_000_100);
        assert_eq!(
            own.current_at(Some(1_000_000_000)).as_deref(),
            Some("aaaaaaaaaaaaaaaa")
        );
        own.record("bbbbbbbbbbbbbbbb", 1_000_003_700);
        assert_eq!(
            own.current_at(Some(1_000_000_000)).as_deref(),
            Some("bbbbbbbbbbbbbbbb")
        );
        // `keep_token_id` is read at call time, so once the newest has lapsed
        // the answer falls back to a live predecessor rather than naming a
        // dead token.
        own.record("cccccccccccccccc", 1_000_000_050);
        assert_eq!(
            own.current_at(Some(1_000_000_060)).as_deref(),
            Some("bbbbbbbbbbbbbbbb")
        );
    }

    #[test]
    fn prune_drops_only_lapsed_ids_and_a_blind_clock_prunes_nothing() {
        let mut own = OwnSessionIds::default();
        own.record("aaaaaaaaaaaaaaaa", 1_000_000_100);
        own.record("bbbbbbbbbbbbbbbb", 1_000_003_700);
        // A failed clock read must not empty the set: dropping a live own id
        // paints the app's own session as a stranger.
        own.prune(None);
        assert_eq!(own.ids_at(None).len(), 2);
        own.prune(Some(1_000_000_200));
        assert_eq!(own.ids_at(None), vec!["bbbbbbbbbbbbbbbb".to_string()]);
    }

    #[test]
    fn re_recording_an_id_refreshes_it_rather_than_duplicating_it() {
        let mut own = OwnSessionIds::default();
        own.record("aaaaaaaaaaaaaaaa", 1_000_000_100);
        own.record("bbbbbbbbbbbbbbbb", 1_000_000_200);
        own.record("aaaaaaaaaaaaaaaa", 1_000_003_700);
        // One entry per session, and the re-recorded one is now the current.
        assert_eq!(
            own.ids_at(Some(1_000_000_000)),
            vec![
                "bbbbbbbbbbbbbbbb".to_string(),
                "aaaaaaaaaaaaaaaa".to_string()
            ]
        );
        assert_eq!(
            own.current_at(Some(1_000_000_000)).as_deref(),
            Some("aaaaaaaaaaaaaaaa")
        );
    }

    #[test]
    fn an_empty_set_names_no_session() {
        let own = OwnSessionIds::default();
        assert!(own.is_empty_at(Some(1_000_000_000)));
        assert!(own.ids_at(Some(1_000_000_000)).is_empty());
        assert_eq!(own.current_at(Some(1_000_000_000)), None);
    }

    #[test]
    fn handshake_signed_message_binds_the_nest_before_the_nonce_tail() {
        let actor = [0xab_u8; 32];
        let ts: u64 = 1_700_000_000_000;
        let nest = [0x5e_u8; 32];
        let nonce = [0x42_u8; 32];
        let tag = crate::sig_domain::AUTH_HANDSHAKE_V2;
        // AUTH_HANDSHAKE_V2 ‖ actor_id ‖ timestamp_be ‖ nest_id ‖ client_nonce.
        let m = handshake_signed_message(&actor, ts, &nest, &nonce);
        assert_eq!(m.len(), tag.len() + 104);
        assert!(m.starts_with(tag));
        assert_eq!(&m[tag.len()..tag.len() + 32], &actor);
        assert_eq!(&m[tag.len() + 32..tag.len() + 40], &ts.to_be_bytes());
        assert_eq!(&m[tag.len() + 40..tag.len() + 72], &nest);
        assert_eq!(&m[tag.len() + 72..], &nonce);
        // Distinct nonces ⇒ distinct signed messages (the collision fix).
        assert_ne!(
            m,
            handshake_signed_message(&actor, ts, &nest, &[0x99_u8; 32])
        );
        // Distinct nests ⇒ distinct signed messages: the blob signed for one
        // nest is not the blob another nest verifies (`login.md` § Binding
        // the nest) — the whole point of the reset.
        assert_ne!(
            m,
            handshake_signed_message(&actor, ts, &[0x5f_u8; 32], &nonce)
        );
        // The tag structurally separates a login message from the retired
        // unbound form and from every other actor-key context (rule #8).
        let mut unbound = Vec::with_capacity(72);
        unbound.extend_from_slice(&actor);
        unbound.extend_from_slice(&ts.to_be_bytes());
        unbound.extend_from_slice(&nonce);
        assert_ne!(
            m,
            crate::sig_domain::domain_separated(crate::sig_domain::AUTH_HANDSHAKE_V1, &unbound)
        );
        assert_ne!(m, crate::claim::claim_admin_signed_message(&actor, ts));
        assert_ne!(
            m,
            crate::account::account_lockout_signed_message(&actor, ts)
        );
    }

    #[test]
    fn challenge_verify_signed_message_is_tagged_actor_nonce_then_nest() {
        let actor = [0xab_u8; 32];
        let nonce = [0x55_u8; 32];
        let nest = [0x5e_u8; 32];
        let tag = crate::sig_domain::AUTH_VERIFY_V2;
        let m = challenge_verify_signed_message(&actor, &nonce, &nest);
        assert_eq!(m.len(), tag.len() + 96);
        assert!(m.starts_with(tag));
        assert_eq!(&m[tag.len()..tag.len() + 32], &actor);
        assert_eq!(&m[tag.len() + 32..tag.len() + 64], &nonce);
        assert_eq!(&m[tag.len() + 64..], &nest);
        // A verify signed for one nest is not the message another verifies.
        assert_ne!(
            m,
            challenge_verify_signed_message(&actor, &nonce, &[0x5f_u8; 32])
        );
        // Never confusable with the retired unbound form, nor with a login
        // handshake over the same actor (the cross-context rule-#8 guarantee).
        let mut unbound = Vec::with_capacity(64);
        unbound.extend_from_slice(&actor);
        unbound.extend_from_slice(&nonce);
        assert_ne!(
            m,
            crate::sig_domain::domain_separated(crate::sig_domain::AUTH_VERIFY_V1, &unbound)
        );
        assert_ne!(m, handshake_signed_message(&actor, u64::MAX, &nest, &nonce));
    }

    #[test]
    fn device_and_custody_handshakes_bind_the_nest_too() {
        let actor = [0xab_u8; 32];
        let key = [0xcd_u8; 32];
        let ts: u64 = 1_700_000_000_000;
        let (nest_a, nest_b) = ([0x5e_u8; 32], [0x5f_u8; 32]);
        let nonce = [0x42_u8; 32];
        let d = device_handshake_signed_message(&actor, &key, ts, &nest_a, &nonce);
        assert!(d.starts_with(crate::sig_domain::DEVICE_HANDSHAKE_V2));
        assert_ne!(
            d,
            device_handshake_signed_message(&actor, &key, ts, &nest_b, &nonce)
        );
        let c = custody_handshake_signed_message(&actor, &key, ts, &nest_a, &nonce);
        assert!(c.starts_with(crate::sig_domain::CUSTODY_HANDSHAKE_V2));
        assert_ne!(
            c,
            custody_handshake_signed_message(&actor, &key, ts, &nest_b, &nonce)
        );
        // Same layout, different tag — the standing tag-per-context rule.
        assert_ne!(d, c);
    }

    #[test]
    fn cert_binding_carries_only_the_tagged_signature() {
        // A binding carries the domain-tagged signature and nothing else; it round-trips through canonical CBOR.
        let binding = CertBinding {
            nest_actor_id: "ab".repeat(32),
            spki_sha256: ByteBuf::from(vec![0x11u8; 32]),
            tagged_sig: ByteBuf::from(vec![0x33u8; 64]),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&binding).unwrap();
        let back: CertBinding = decode(&bytes).unwrap();
        assert_eq!(back, binding);

        // The pre-tag shape — an untagged `sig` and no `tagged_sig` — is not a
        // binding any more: it fails to decode rather than being verified over
        // bytes the deployment key never tags (the compat half was removed
        // 2026-09-24; `version-compatibility.md` § Dimension 2).
        let untagged = crate::Value::Map(
            [
                ("nest_actor_id", crate::Value::String("ab".repeat(32))),
                ("spki_sha256", crate::Value::Bytes(vec![0x11u8; 32])),
                ("sig", crate::Value::Bytes(vec![0x22u8; 64])),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        );
        let untagged_bytes = encode_canonical(&untagged).unwrap();
        assert!(decode::<CertBinding>(&untagged_bytes).is_err());
    }

    /// The one producer every fixture and the nest share signs exactly
    /// [`cert_binding_signed_message`] — so a forged nest in a test and the
    /// real handler can never drift apart on the bytes.
    #[test]
    fn cert_binding_sign_covers_the_single_source_message() {
        // verify-ok(test): signs with a fixed local key and checks its own
        // signature — the key is never attacker-chosen.
        use ed25519_dalek::Verifier;
        let key = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]);
        let spki = [0x11u8; 32];
        let nonce = [0x22u8; 32];
        let binding = CertBinding::sign(&key, &spki, &nonce);
        assert_eq!(
            binding.nest_actor_id,
            hex::encode(key.verifying_key().to_bytes())
        );
        assert_eq!(binding.spki_sha256.as_slice(), &spki);
        let sig = ed25519_dalek::Signature::from_slice(binding.tagged_sig.as_slice()).unwrap();
        key.verifying_key()
            .verify(&cert_binding_signed_message(&spki, &nonce), &sig)
            .expect("the tagged signature covers the single-source message");
        assert!(
            cert_binding_signed_message(&spki, &nonce)
                .starts_with(crate::sig_domain::CERT_BINDING_V1)
        );
    }

    #[test]
    fn handshake_request_round_trips() {
        let req = HandshakeRequest {
            actor_id: "ab".repeat(32),
            timestamp: 1_700_000_000_000,
            signature: "cd".repeat(64),
            client_nonce: ByteBuf::from(vec![0x42u8; 32]),
            nest_id: "5e".repeat(32),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: HandshakeRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
        assert_eq!(decoded.client_nonce.as_slice(), &[0x42u8; 32][..]);
    }

    /// The unbound request shape is not a request any more: `nest_id` and
    /// `client_nonce` are required fields, so a signer that predates the
    /// binding fails to decode rather than being verified against bytes it
    /// did not sign (`login.md` § Binding the nest — no accept path kept).
    #[test]
    fn an_unbound_handshake_request_does_not_decode() {
        let unbound = crate::Value::Map(
            [
                ("actor_id", crate::Value::String("ab".repeat(32))),
                ("timestamp", crate::Value::Integer(1_700_000_000_000)),
                ("signature", crate::Value::String("cd".repeat(64))),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        );
        let bytes = encode_canonical(&unbound).unwrap();
        assert!(decode::<HandshakeRequest>(&bytes).is_err());
    }

    #[test]
    fn handshake_reply_canonical_re_encodes_identically() {
        let reply = HandshakeReply {
            token: "deadbeef.cafef00d".into(),
            token_id: "0011223344556677".into(),
            expires_at: 1_700_000_003_600,
            expires_in: 3600,
            cert_binding: None,
            extra: BTreeMap::new(),
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: HandshakeReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn handshake_reply_round_trips_with_cert_binding() {
        let reply = HandshakeReply {
            token: "deadbeef.cafef00d".into(),
            token_id: "0011223344556677".into(),
            expires_at: 1_700_000_003_600,
            expires_in: 3600,
            cert_binding: Some(CertBinding {
                nest_actor_id: "ef".repeat(32),
                spki_sha256: ByteBuf::from(vec![0xaau8; 32]),
                tagged_sig: ByteBuf::from(vec![0xccu8; 64]),
                extra: Default::default(),
            }),
            extra: BTreeMap::new(),
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: HandshakeReply = decode(&bytes1).unwrap();
        assert_eq!(reply, decoded);
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    /// A binding-less reply (a plain-HTTP dev nest) decodes on a client that
    /// knows the field — the binding is optional by nest posture, not by
    /// version.
    #[test]
    fn handshake_reply_without_binding_decodes() {
        let unbound = HandshakeReply {
            token: "t".into(),
            token_id: "0011223344556677".into(),
            expires_at: 1,
            expires_in: 1,
            cert_binding: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&unbound).unwrap();
        let decoded: HandshakeReply = decode(&bytes).unwrap();
        assert!(decoded.cert_binding.is_none());
    }

    /// `expires_in` is required: a reply that carries it round-trips it — the
    /// field a client anchors its refresh schedule to on its own clock — and a
    /// reply without it (the pre-2026-09-21 shape) is refused at decode rather
    /// than read through an `expires_at` fallback (the fallback retired
    /// 2026-09-24 with the compat-remnant sweep).
    #[test]
    fn a_handshake_reply_without_expires_in_does_not_decode() {
        let reply = HandshakeReply {
            token: "t".into(),
            token_id: "0011223344556677".into(),
            expires_at: 1_700_000_003_600,
            expires_in: 3600,
            cert_binding: None,
            extra: BTreeMap::new(),
        };
        let decoded: HandshakeReply = decode(&encode_canonical(&reply).unwrap()).unwrap();
        assert_eq!(decoded.expires_in, 3600);

        let without = crate::Value::Map(
            [
                ("token", crate::Value::String("t".into())),
                ("token_id", crate::Value::String("0011223344556677".into())),
                ("expires_at", crate::Value::Integer(1_700_000_003_600)),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        );
        assert!(decode::<HandshakeReply>(&encode_canonical(&without).unwrap()).is_err());
    }

    /// The one conversion every bearer holder makes at receipt: the deadline
    /// is anchored to the CLIENT's clock, whatever the nest's `expires_at`
    /// says; only with no readable clock does the nest-absolute `expires_at`
    /// stand.
    #[test]
    fn the_own_clock_deadline_anchors_expires_in_to_the_client_and_falls_back_otherwise() {
        // A client six hours ahead of the nest: the nest's absolute deadline is
        // already "in the past" on this clock, but the anchored one is a full
        // TTL away.
        let nest_expires_at = 1_700_003_600;
        let client_now = 1_700_000_000 + 6 * 3600;
        assert_eq!(
            deadline_on_own_clock(Some(client_now), 3600, nest_expires_at),
            client_now + 3600
        );
        // Unreadable client clock: nothing to anchor to.
        assert_eq!(
            deadline_on_own_clock(None, 3600, nest_expires_at),
            nest_expires_at
        );
        assert_eq!(
            deadline_on_own_clock(Some(u64::MAX), 3600, nest_expires_at),
            u64::MAX,
            "saturates rather than wrapping"
        );
    }

    #[test]
    fn nest_handshake_request_round_trips() {
        let req = NestHandshakeRequest {
            client_nonce: ByteBuf::from(vec![0x42u8; 32]),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: NestHandshakeRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
        assert_eq!(decoded.client_nonce.as_slice(), &[0x42u8; 32][..]);
    }

    #[test]
    fn nest_handshake_reply_round_trips_with_cert_binding() {
        let reply = NestHandshakeReply {
            cert_binding: Some(CertBinding {
                nest_actor_id: "ef".repeat(32),
                spki_sha256: ByteBuf::from(vec![0xaau8; 32]),
                tagged_sig: ByteBuf::from(vec![0xccu8; 64]),
                extra: Default::default(),
            }),
            extra: BTreeMap::new(),
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: NestHandshakeReply = decode(&bytes1).unwrap();
        assert_eq!(reply, decoded);
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    /// A binding-less reply (plain-HTTP dev nest / keyless nest) must decode as
    /// `None` and re-encode without the key — the client's "nothing to bind"
    /// signal. A peer still sending the retired `nat_mode_v2` advert is
    /// absorbed by the catch-all, never refused.
    #[test]
    fn nest_handshake_reply_without_binding_decodes() {
        let reply = NestHandshakeReply {
            cert_binding: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: NestHandshakeReply = decode(&bytes).unwrap();
        assert!(decoded.cert_binding.is_none());

        let with_retired_advert = crate::Value::Map(
            [("nat_mode_v2", crate::Value::Bool(true))]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        );
        let decoded: NestHandshakeReply =
            decode(&encode_canonical(&with_retired_advert).unwrap()).unwrap();
        assert_eq!(
            decoded.extra.get("nat_mode_v2"),
            Some(&crate::Value::Bool(true))
        );
    }

    #[test]
    fn challenge_request_round_trips() {
        let req = ChallengeRequest {
            actor_id: "11".repeat(32),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ChallengeRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn challenge_reply_round_trips() {
        let reply = ChallengeReply {
            nonce: "22".repeat(32),
            expires_in: 300,
            expires_at: 1_700_000_000,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ChallengeReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn verify_request_round_trips() {
        let req = VerifyRequest {
            actor_id: "33".repeat(32),
            nonce: "44".repeat(32),
            signature: "55".repeat(64),
            client_nonce: Some(ByteBuf::from(vec![0x66u8; 32])),
            nest_id: "5e".repeat(32),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: VerifyRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
        assert_eq!(decoded.client_nonce.unwrap().as_slice(), &[0x66u8; 32][..]);

        // A request without the NT-1 client nonce still decodes (the binding
        // fold is optional); one without `nest_id` does not — the nest
        // binding is mandatory (`login.md` § Binding the nest).
        let no_client_nonce = VerifyRequest {
            actor_id: "33".repeat(32),
            nonce: "44".repeat(32),
            signature: "55".repeat(64),
            client_nonce: None,
            nest_id: "5e".repeat(32),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&no_client_nonce).unwrap();
        let decoded: VerifyRequest = decode(&bytes).unwrap();
        assert_eq!(no_client_nonce, decoded);
        assert!(decoded.client_nonce.is_none());
        let unbound = crate::Value::Map(
            [
                ("actor_id", crate::Value::String("33".repeat(32))),
                ("nonce", crate::Value::String("44".repeat(32))),
                ("signature", crate::Value::String("55".repeat(64))),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        );
        let bytes = encode_canonical(&unbound).unwrap();
        assert!(decode::<VerifyRequest>(&bytes).is_err());
    }

    #[test]
    fn verify_reply_round_trips_and_re_encodes_identically() {
        let reply = VerifyReply {
            token: "tok.123".into(),
            token_id: "0011223344556677".into(),
            handle: "alice".into(),
            domain: "nest.example".into(),
            tier: "free".into(),
            expires_at: 1_700_000_003_600,
            ..Default::default()
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: VerifyReply = decode(&bytes1).unwrap();
        assert_eq!(reply, decoded);
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    /// The silent-challenge (`fauna.auth.verify`) reply carries the same Axis-1
    /// channel-binding proof as the direct handshake — the web launch path's
    /// possession-verify + TOFU-pin source (security.md § Transport trust).
    #[test]
    fn verify_reply_round_trips_with_cert_binding() {
        let reply = VerifyReply {
            token: "tok.123".into(),
            token_id: "0011223344556677".into(),
            handle: "alice".into(),
            domain: "nest.example".into(),
            tier: "free".into(),
            expires_at: 1_700_000_003_600,
            cert_binding: Some(Box::new(CertBinding {
                nest_actor_id: "ef".repeat(32),
                spki_sha256: ByteBuf::from(vec![0xaau8; 32]),
                tagged_sig: ByteBuf::from(vec![0xccu8; 64]),
                extra: Default::default(),
            })),
            ..Default::default()
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: VerifyReply = decode(&bytes1).unwrap();
        assert_eq!(reply, decoded);
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    /// A verify reply without `cert_binding` (plain-HTTP or keyless nest) must
    /// decode: the binding is optional.
    #[test]
    fn verify_reply_without_binding_decodes() {
        let unbound = VerifyReply {
            token: "t".into(),
            token_id: "8899aabbccddeeff".into(),
            domain: "localhost".into(),
            tier: "free".into(),
            expires_at: 1,
            ..Default::default()
        };
        let bytes = encode_canonical(&unbound).unwrap();
        let decoded: VerifyReply = decode(&bytes).unwrap();
        assert!(decoded.cert_binding.is_none());
    }

    #[test]
    fn verify_reply_tolerates_empty_handle() {
        let reply = VerifyReply {
            token: "t".into(),
            token_id: "8899aabbccddeeff".into(),
            domain: "localhost".into(),
            tier: "free".into(),
            expires_at: 1,
            ..Default::default()
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: VerifyReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    /// The retired `storage_mode_pending` flag (any stray key) is absorbed by
    /// the catch-all, never refused, and a
    /// reply without `expires_in` — the pre-2026-09-21 shape — is refused at
    /// decode rather than read through an `expires_at` fallback.
    #[test]
    fn a_verify_reply_absorbs_the_retired_flag_and_requires_expires_in() {
        let reply = VerifyReply {
            token: "t".into(),
            token_id: "8899aabbccddeeff".into(),
            handle: "admin".into(),
            domain: "localhost".into(),
            tier: "free".into(),
            expires_at: 1,
            expires_in: 1,
            ..Default::default()
        };
        let as_map = |r: &VerifyReply| -> BTreeMap<String, crate::Value> {
            match decode::<crate::Value>(&encode_canonical(r).unwrap()).unwrap() {
                crate::Value::Map(m) => m,
                other => panic!("a reply encodes as a map, got {other:?}"),
            }
        };
        let mut with_retired = as_map(&reply);
        with_retired.insert("storage_mode_pending".into(), crate::Value::Bool(false));
        let decoded: VerifyReply =
            decode(&encode_canonical(&crate::Value::Map(with_retired)).unwrap()).unwrap();
        assert_eq!(
            decoded.extra.get("storage_mode_pending"),
            Some(&crate::Value::Bool(false))
        );

        let mut without = as_map(&reply);
        without.remove("expires_in");
        assert!(
            decode::<VerifyReply>(&encode_canonical(&crate::Value::Map(without)).unwrap()).is_err()
        );
    }

    /// `run_silent_challenge` ceremony + error mapping over a mock
    /// `RpcRequester` (no nest, no network). The real WS round-trip is proven by
    /// the tier_3 `bins/fauna-nest/tests/launch_machine_auth_roundtrip.rs` and
    /// `onboarding_ws_rpc_roundtrip.rs`.
    #[cfg(feature = "auth-ceremony")]
    mod silent_challenge_ceremony {
        use super::super::*;
        use crate::codec::{decode_strict as decode, encode_canonical};
        use crate::{LocalizedText, RpcError, RpcErrorClass, RpcRequester, Value};
        use std::collections::HashMap;

        /// A canned-reply requester: maps each kind to a pre-built reply `Value`
        /// (encoded from a typed reply) or a wire `RpcError`. Decodes the stored
        /// `Value` into the caller's `Reply` exactly like the real connectors.
        ///
        /// **Deliberately NOT `fauna_client_testkit::RejectingRequester`, which
        /// is this double lifted** (`fauna-launch-machine` and
        /// `fauna-media-machine` took it; this crate cannot). Adopting it here
        /// needs a dev-dependency cycle — the testkit depends on this crate —
        /// and while Cargo permits that, it does not typecheck: the testkit
        /// takes `fauna-protocol` with `default-features = false`, so a test
        /// build that turns on `auth-ceremony` puts **two different instances
        /// of `fauna_protocol` in one graph**, and the double ends up
        /// implementing the *other* `RpcRequester`. Measured 2026-08-23 —
        /// `error[E0277] ... note: there are multiple different versions of
        /// crate `fauna_protocol` in the dependency graph`, 25 of them.
        ///
        /// Moving the doubles *down* into this crate behind a feature is the
        /// other obvious escape, and `fauna-client-testkit`'s own crate docs
        /// reject it on purpose (§ "Why a separate crate rather than a feature
        /// on `fauna-protocol`": a dev-dep-only crate keeps test scaffolding
        /// out of release artifacts *by construction*, with no compile-time
        /// gate to get wrong). So this copy stays, and the classification rule
        /// it encodes is pinned by the testkit's own tests as well.
        struct MockRequester {
            responses: HashMap<&'static str, Result<Value, RpcError>>,
        }

        #[derive(Debug)]
        enum MockErr {
            Rpc(RpcError),
            Transport(String),
        }

        impl std::fmt::Display for MockErr {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    MockErr::Rpc(e) => write!(f, "rpc: {}", e.code),
                    MockErr::Transport(s) => write!(f, "transport: {s}"),
                }
            }
        }

        impl RpcErrorClass for MockErr {
            fn as_rpc_error(&self) -> Option<&RpcError> {
                match self {
                    MockErr::Rpc(e) => Some(e),
                    MockErr::Transport(_) => None,
                }
            }
            fn is_rejection(&self) -> bool {
                self.as_rpc_error().is_some()
            }
        }

        impl RpcRequester for MockRequester {
            type Error = MockErr;
            async fn request<Req, Reply>(
                &self,
                kind: &'static str,
                _payload: Req,
            ) -> Result<Reply, MockErr>
            where
                Req: serde::Serialize,
                Reply: serde::de::DeserializeOwned,
            {
                match self.responses.get(kind) {
                    Some(Ok(v)) => {
                        let bytes = encode_canonical(v).expect("encode canned value");
                        Ok(decode(&bytes).expect("decode reply"))
                    }
                    Some(Err(e)) => Err(MockErr::Rpc(e.clone())),
                    None => Err(MockErr::Transport(format!("no mock for {kind}"))),
                }
            }
        }

        fn to_value<T: serde::Serialize>(t: &T) -> Value {
            let bytes = encode_canonical(t).unwrap();
            decode(&bytes).unwrap()
        }

        fn rpc(code: &str) -> RpcError {
            RpcError {
                code: code.into(),
                message: Box::new(LocalizedText::new("error.x")),
                details: None,
                extra: Default::default(),
            }
        }

        const SECRET: [u8; 32] = [0x42; 32];
        /// The identity the canned nest "proves" — the ceremony binds it into
        /// the signature; the canned replies never verify anything.
        const NEST: [u8; 32] = [0x77; 32];

        fn challenge_reply() -> ChallengeReply {
            ChallengeReply {
                nonce: hex::encode([0x55u8; 32]),
                expires_in: 300,
                expires_at: 1_700_000_000,
                extra: Default::default(),
            }
        }

        #[tokio::test]
        async fn happy_path_returns_verify_reply() {
            let mut responses = HashMap::new();
            responses.insert(CHALLENGE_KIND, Ok(to_value(&challenge_reply())));
            responses.insert(
                VERIFY_KIND,
                Ok(to_value(&VerifyReply {
                    token: "actor.opaque".into(),
                    token_id: "0011223344556677".into(),
                    handle: "alice".into(),
                    domain: "nest.example".into(),
                    tier: "free".into(),
                    expires_at: 1_700_003_600,
                    ..Default::default()
                })),
            );
            let m = MockRequester { responses };
            match run_silent_challenge(&m, &SECRET, &NEST).await {
                SilentChallengeOutcome::Success(v) => {
                    assert_eq!(v.token, "actor.opaque");
                    assert_eq!(v.handle, "alice");
                }
                other => panic!("expected Success, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn verify_not_registered_maps_to_not_registered() {
            let mut responses = HashMap::new();
            responses.insert(CHALLENGE_KIND, Ok(to_value(&challenge_reply())));
            responses.insert(VERIFY_KIND, Err(rpc("fauna.auth.not_registered")));
            let m = MockRequester { responses };
            match run_silent_challenge(&m, &SECRET, &NEST).await {
                SilentChallengeOutcome::NotRegistered => {}
                other => panic!("expected NotRegistered, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn verify_account_locked_maps_to_locked_with_its_time() {
            let mut responses = HashMap::new();
            responses.insert(CHALLENGE_KIND, Ok(to_value(&challenge_reply())));
            let mut locked = rpc("fauna.auth.account_locked");
            locked.details = Some(Box::new(crate::Value::Integer(1_700_086_400)));
            responses.insert(VERIFY_KIND, Err(locked));
            let m = MockRequester { responses };
            match run_silent_challenge(&m, &SECRET, &NEST).await {
                SilentChallengeOutcome::Locked { locked_until_secs } => {
                    assert_eq!(locked_until_secs, 1_700_086_400)
                }
                other => panic!("expected Locked, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn account_locked_without_a_readable_time_stays_transient() {
            let mut responses = HashMap::new();
            responses.insert(CHALLENGE_KIND, Ok(to_value(&challenge_reply())));
            responses.insert(VERIFY_KIND, Err(rpc("fauna.auth.account_locked")));
            let m = MockRequester { responses };
            match run_silent_challenge(&m, &SECRET, &NEST).await {
                SilentChallengeOutcome::Transient { .. } => {}
                other => panic!("expected Transient, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn challenge_failure_is_transient() {
            // challenge errors (transport) → Transient, verify never runs.
            let m = MockRequester {
                responses: HashMap::new(),
            };
            match run_silent_challenge(&m, &SECRET, &NEST).await {
                SilentChallengeOutcome::Transient { .. } => {}
                other => panic!("expected Transient, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn nest_outdated_at_challenge_is_needs_update() {
            // A degraded nest rejects the very first round-trip with
            // `fauna.nest.outdated`. Must surface an update prompt, not retry
            // (version-compatibility.md Dim 4).
            let mut responses = HashMap::new();
            responses.insert(CHALLENGE_KIND, Err(RpcError::nest_outdated()));
            let m = MockRequester { responses };
            match run_silent_challenge(&m, &SECRET, &NEST).await {
                SilentChallengeOutcome::NeedsUpdate { message } => {
                    assert_eq!(message, RpcError::nest_outdated().localized());
                }
                other => panic!("expected NeedsUpdate, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn nest_outdated_at_verify_is_needs_update() {
            let mut responses = HashMap::new();
            responses.insert(CHALLENGE_KIND, Ok(to_value(&challenge_reply())));
            responses.insert(VERIFY_KIND, Err(RpcError::nest_outdated()));
            let m = MockRequester { responses };
            match run_silent_challenge(&m, &SECRET, &NEST).await {
                SilentChallengeOutcome::NeedsUpdate { message } => {
                    assert_eq!(message, RpcError::nest_outdated().localized());
                }
                other => panic!("expected NeedsUpdate, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn invalid_nonce_on_verify_is_transient() {
            // A consumed/expired nonce is retryable — a retry gets a fresh one.
            let mut responses = HashMap::new();
            responses.insert(CHALLENGE_KIND, Ok(to_value(&challenge_reply())));
            responses.insert(VERIFY_KIND, Err(rpc("fauna.auth.invalid_nonce")));
            let m = MockRequester { responses };
            match run_silent_challenge(&m, &SECRET, &NEST).await {
                SilentChallengeOutcome::Transient { .. } => {}
                other => panic!("expected Transient, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn bad_secret_length_is_secret_invalid() {
            let m = MockRequester {
                responses: HashMap::new(),
            };
            match run_silent_challenge(&m, &[0xaa; 16], &NEST).await {
                SilentChallengeOutcome::SecretInvalid { .. } => {}
                other => panic!("expected SecretInvalid, got {other:?}"),
            }
        }

        /// The signed challenge message is the tagged `actor_id ‖ nonce ‖ nest_id` body
        /// (no timestamp) — guard the wire contract so a future refactor can't
        /// silently reintroduce the timestamp the direct-auth path uses, and
        /// pin that the silent challenge signs the same bytes the single-source
        /// builder produces.
        #[test]
        fn challenge_signed_message_is_actor_id_then_nonce() {
            // verify-ok(test): this module signs with a locally generated key and checks
            // its own signature back — no wire-supplied key reaches it, so the permissive
            // trait is harmless here. Production verification goes through
            // `fauna_core::identity::verify_detached`; the walk guard
            // `fauna-core/tests/one_ed25519_verification_shape.rs` reads this marker.
            use ed25519_dalek::{Signer, SigningKey, Verifier};
            let signing = SigningKey::from_bytes(&SECRET);
            let actor_id = signing.verifying_key().to_bytes();
            let nonce = [0x55u8; 32];
            let msg = crate::auth::challenge_verify_signed_message(&actor_id, &nonce, &NEST);
            let tag = crate::sig_domain::AUTH_VERIFY_V2;
            assert_eq!(msg.len(), tag.len() + 96);
            assert!(msg.starts_with(tag));
            let sig = signing.sign(&msg);
            assert!(signing.verifying_key().verify(&msg, &sig).is_ok());
        }
    }
}
