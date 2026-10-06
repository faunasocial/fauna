//! Wire types for the `fauna.bridges.atproto.*` F1 auth-core surface —
//! app-credential custody, session registry, and the per-account
//! external-apps kill-switch (`docs/goal/behavior/atproto-pds-full.md`
//! § Detailed design → *WS-RPC kind surface* / *F1 detail*).
//!
//! Two caller classes share this namespace (enforced nest-side in
//! `bridge_method_allowlist`): the attested `atproto.pds` bridge calls the
//! verifier-fetch/session-registry kinds; Fauna apps (User, Admin ⊇ User)
//! call the mint/list/revoke/kill-switch kinds. All timestamps are epoch
//! **milliseconds** (the nest `now_epoch_millis` convention). Every struct
//! carries the standard forward-compat `extra` flatten (transport.md
//! § Schema and forward-compat discipline, rule 4).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use crate::Value;

// ── Bridge-class: verifier fetch (createSession) ────────────────

/// `fauna.bridges.atproto.fetch_app_credential_verifiers` — the bridge
/// resolves a login identifier to the account's verifier rows at
/// `createSession`. Verification itself happens bridge-side (Argon2id over
/// the PHC strings below), keeping the secret and the KDF cost off the nest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchAppCredentialVerifiersRequest {
    /// The identifier the XRPC client presented: an ATProto handle
    /// (bare Fauna handle or `handle.domain` form) or a DID. Both resolve
    /// since slice 4d — a DID matches the `atproto_identities` row that
    /// stores it, which is the same value the account's session `sub`
    /// carries, so an app can log back in with the DID it was handed.
    pub identifier: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One stored app-credential verifier row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppCredentialVerifierRow {
    pub credential_id: String,
    /// PHC-serialized Argon2id verifier, client-computed at mint.
    pub verifier: String,
    /// Whether this credential unlocks the DM-privileged scope variant.
    pub dm_allowed: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchAppCredentialVerifiersReply {
    /// `Some(32-byte actor id)` when the identifier resolved to a local
    /// account; `None` otherwise (the bridge still fails uniformly toward
    /// the XRPC client — no enumeration signal leaves the box).
    pub actor_id: Option<ByteBuf>,
    /// The account's external-apps kill-switch (default ON). Rides along
    /// so the bridge needs no second fetch; refreshed by the
    /// `sessions_changed` nudge on a flip.
    pub external_apps_enabled: bool,
    /// The DID this account may open a session as — its real ATProto DID,
    /// and `None` when it has no **active** hosted identity with a minted
    /// DID (slice 4d).
    ///
    /// The bridge mints the session token's `sub` from this and never
    /// derives a DID of its own; F1's `did:fauna:<hex>` placeholder is what
    /// put the write path and the projection loop in two different repos.
    /// The active-and-minted rule lives here rather than bridge-side for the
    /// same reason D8's planes do: identity status is *interpretation*, and
    /// interpretation stays out of Go. `None` is a uniform auth failure.
    #[serde(default)]
    pub login_did: Option<String>,
    pub verifiers: Vec<AppCredentialVerifierRow>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── Bridge-class: session registry ──────────────────────────────

/// `fauna.bridges.atproto.record_session` — register a freshly-minted
/// session (post-verify) so Fauna apps can list/revoke it (D4: the
/// registry makes revocation *visible*). Also stamps `last_used_at` on the
/// minting credential. Refused (`fauna.bridges.atproto.disabled`) when the
/// account's kill-switch is OFF.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordSessionRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// The refresh-token family id: the *initial* refresh `jti`. Stays
    /// constant across rotate-on-use; the current jti lives nest-side and
    /// rotates via `refresh_session`.
    #[serde(with = "serde_bytes")]
    pub session_id: Vec<u8>,
    /// `"app_credential"` today; `"oauth"` when F4 lands.
    pub plane: String,
    /// The minting credential (app-credential plane only).
    pub credential_id: Option<String>,
    /// Free-text client hint for the sessions list (user-visible).
    pub client_note: Option<String>,
    /// Refresh-token expiry, epoch millis.
    pub expires_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordSessionReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.refresh_session` — validate + rotate the registry
/// row for a presented refresh token (rotate-on-use). Reuse of a superseded
/// jti kills the whole session family (the one-row reuse check, F1 detail).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RefreshSessionRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub session_id: Vec<u8>,
    /// The jti the client presented (must equal the stored current jti).
    #[serde(with = "serde_bytes")]
    pub presented_jti: Vec<u8>,
    /// The replacement jti minted for this rotation.
    #[serde(with = "serde_bytes")]
    pub new_jti: Vec<u8>,
    /// New refresh expiry, epoch millis.
    pub new_expires_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `RefreshSessionReply.status` values (strings on the wire for
/// cross-language forward-compat; the Go bridge switches on these).
pub mod refresh_status {
    /// Rotation succeeded; the new jti is live.
    pub const ROTATED: &str = "rotated";
    /// The presented jti was already superseded — replay detected; the
    /// whole session family is now revoked.
    pub const REUSE_DETECTED: &str = "reuse_detected";
    /// No such session (never existed, or already revoked/expired).
    pub const INVALID: &str = "invalid";
    /// The account's external-apps kill-switch is OFF.
    pub const DISABLED: &str = "disabled";
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RefreshSessionReply {
    /// One of [`refresh_status`].
    pub status: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.end_session` — `deleteSession`: revoke the row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EndSessionRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub session_id: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EndSessionReply {
    /// False when the session was already gone (idempotent).
    pub ended: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── User-class: credential mint / list / revoke ─────────────────

/// `fauna.bridges.atproto.provision_app_credential` — self-scoped: the
/// authenticated client stores the PHC verifier it computed at mint (the
/// nest never sees the secret). The recoverable copy lives in the minting
/// client's account plane (`fauna.state.atproto`), exactly like mail credentials.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisionAppCredentialRequest {
    pub credential_id: String,
    /// User-visible label ("tapbots-ivory", …), kebab-derived like mail.
    pub label: String,
    /// PHC-serialized Argon2id verifier.
    pub verifier: String,
    pub dm_allowed: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisionAppCredentialReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One credential row as the settings UI lists it (no verifier material).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppCredentialInfo {
    pub credential_id: String,
    pub label: String,
    pub dm_allowed: bool,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListAppCredentialsRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListAppCredentialsReply {
    pub credentials: Vec<AppCredentialInfo>,
    /// The account's kill-switch state rides along so the settings page
    /// renders the toggle from the same fetch it lists credentials with.
    pub external_apps_enabled: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.revoke_app_credential` — self-scoped; cascades:
/// every session minted from the credential is revoked and the bridge is
/// nudged (`sessions_changed`) to drop cached state immediately.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokeAppCredentialRequest {
    pub credential_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokeAppCredentialReply {
    /// False when no such credential existed (idempotent).
    pub revoked: bool,
    /// Sessions killed by the cascade.
    pub sessions_revoked: u32,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── User-class: session list / revoke ───────────────────────────

/// One live session row as the settings UI lists it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AtprotoSessionInfo {
    #[serde(with = "serde_bytes")]
    pub session_id: Vec<u8>,
    pub plane: String,
    pub credential_id: Option<String>,
    pub client_note: Option<String>,
    pub created_at: i64,
    pub last_refreshed_at: Option<i64>,
    pub expires_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListSessionsRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListSessionsReply {
    /// Live (unrevoked, unexpired) sessions only.
    pub sessions: Vec<AtprotoSessionInfo>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokeSessionRequest {
    #[serde(with = "serde_bytes")]
    pub session_id: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokeSessionReply {
    pub revoked: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── User-class: the OAuth grant registry read (F4 slice 8a) ─────

/// One OAuth grant as the connected-apps surface lists it — not-yet-revoked,
/// but not necessarily usable right now: see `suspended`.
///
/// The richer half of the connected-apps row: [`AtprotoSessionInfo`] says an
/// OAuth session exists and carries the client's *name*; this says which client
/// id it is, what the user approved it for, and when it was last seen.
///
/// **There is deliberately no `revoke_grant` companion.** `grant_id` *is* the
/// session-family id, so revoking is `revoke_session` with this id — one
/// operation with one spelling, which is what stops the user's app and the
/// bridge's `/oauth/revoke` becoming two implementations of one rule.
// `Default` so fixtures can grow this type with `..Default::default()` — the
// standing prevention for two branches independently adding a field to one wire
// struct and colliding on every hand-listed literal.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AtprotoGrantInfo {
    /// Also the session-family id — what `revoke_session` takes.
    #[serde(with = "serde_bytes")]
    pub grant_id: Vec<u8>,
    /// The client-id URL, rendered verbatim beside the resolved name: it is
    /// the client's self-authenticating identity, and the name is only what a
    /// document at that URL claimed (§ F4 detail — never the self-asserted
    /// string alone).
    pub client_id: String,
    pub client_name: Option<String>,
    /// Space-delimited, the one encoding of a scope set on every surface it
    /// crosses (the consent row, the token claim, the grant registry).
    pub scopes: String,
    /// The permission sets this grant was made from, frozen at the ceremony —
    /// the same provenance the consent card showed, so the connected-apps row
    /// can answer "where did this come from?" a month later. Empty for a grant
    /// that named none, and for every grant recorded before PS-b.
    #[serde(default)]
    pub sets: Vec<ConsentSetInfo>,
    pub created_at: i64,
    /// **Advisory, not forensics** (D10 § Audit's complementary half): the nest
    /// stamps this on a refresh rotation, the only moment it observes the grant
    /// in use. Access-token calls are verified bridge-side and never reach the
    /// nest, so a recent value proves use and an old one proves nothing.
    pub last_used_at: Option<i64>,
    /// Absent for an open-ended (confidential-client) refresh horizon.
    pub expires_at: Option<i64>,
    /// The account's login plane was suspended (a step-down, or
    /// `delete_presence`'s teardown) while this grant's row was deliberately
    /// left at rest — `ui/atproto.md`'s downward matrix: "kept, listed,
    /// individually revocable; stepping back up restores usability." A
    /// suspended grant is not currently usable and carries no live session;
    /// `false` when the key is absent, the same fact a client without a
    /// "suspended" badge already shows.
    #[serde(default)]
    pub suspended: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.list_grants` — the connected-apps registry read
/// (USER class, self-scoped).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListGrantsRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListGrantsReply {
    /// Live (unrevoked, unexpired) grants only — the same liveness predicate
    /// [`ListSessionsReply`] uses, because the two render one surface.
    pub grants: Vec<AtprotoGrantInfo>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── User-class: the external-apps kill-switch ───────────────────

/// `fauna.bridges.atproto.set_external_apps_enabled` — the per-account
/// kill-switch (default ON). OFF suspends the whole external-app plane
/// non-destructively (rows kept, individually revocable); a flip emits
/// `sessions_changed` so the bridge's cached flag updates immediately.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetExternalAppsEnabledRequest {
    pub enabled: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetExternalAppsEnabledReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── User-class: the integration-depth ladder ────────────────────

/// `fauna.bridges.atproto.set_integration_level` request — a USER-class,
/// self-scoped action: the calling actor moves their Bluesky integration to
/// `target_level` (`docs/goal/ui/atproto.md` § Transition semantics).
///
/// **One client call per confirmed transition, atomic, idempotent on retry** is
/// the ratified contract: nest composes every per-rung effect (unlink, mint or
/// reactivate, login-plane open/suspend, layer-2 deactivation) server-side, so
/// a client crash mid-transition leaves the old level or the new level fully
/// realized — never a half-state (§ Where logic lives; the
/// client-state-recoverability invariant).
///
/// The mint parameters are **flat scalars, defaulted**, not a nested optional
/// struct: dag-cbor cannot round-trip nested `Option`s, and they are read only
/// when the transition enters a hosted level with no identity row yet. Entering
/// a hosted level with an existing (possibly deactivated) identity **ignores**
/// them — that path reactivates the same DID and never re-mints.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SetIntegrationLevelRequest {
    /// `"off"`, `"linked"`, `"hosted_visible"` or `"hosted_full"`
    /// ([`crate::atproto::IntegrationLevel`]).
    pub target_level: String,
    /// Mint parameter: `"plc"` (recommended) or `"web"`. Empty when the
    /// transition does not mint.
    pub did_method: String,
    /// Mint parameter: the user-custodied senior rotation key's `did:key`
    /// pubkey (did:plc only). The SECRET never crosses this wire — it lives
    /// only in the user's client credential store.
    pub user_rotation_pub_did_key: String,
    /// Mint parameter: publish the user's existing public posts as history
    /// (the second explicit opt-in — `atproto-pds-bridge.md` § Projection).
    pub history_backfill: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.set_integration_level` reply — the level actually in
/// force after the transition, so a client that raced another device converges
/// on nest's truth rather than on what it asked for.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SetIntegrationLevelReply {
    /// The persisted level after the transition.
    pub level: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.delete_presence` request — the wire behind
/// `atproto-delete-presence` (`docs/goal/ui/atproto.md` § User actions names
/// this kind's slot "S5-scope kind, named at build time"). USER-class and
/// self-scoped: a caller can only ever destroy their own presence.
///
/// It carries no parameters, deliberately. Every choice the flow offers is a
/// choice about *whether* to proceed, which the client's confirm ceremony
/// settles before calling; and the one genuine option inside the flow — the
/// terminal PLC tombstone — is **not** a field here, because it is signed with
/// the user's client-held senior rotation key and submitted to the PLC directory
/// by the client itself. Nest cannot perform it, so nest must not appear to
/// accept it (`atproto-pds-bridge.md` § State & data shape, the key-custody
/// split; `../ui/atproto.md` § Don't do these).
///
/// Idempotent: confirming twice destroys the same presence once.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DeletePresenceRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.delete_presence` reply.
///
/// The reply is deliberately modest about what has happened, because at the
/// instant it is sent only the *decision* is durable: nest has recorded the
/// tombstone and stepped the account down, and the bridge sweeps the records and
/// announces the deletion on its own convergent pass. A reply claiming the
/// records were gone would be a claim nest is in no position to make — and the
/// records the wider network already copied are gone from nobody's cache either
/// (`atproto-pds-bridge.md` § Disable & revocation, the honest caveat).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DeletePresenceReply {
    /// The integration level in force afterwards — always `"off"`: a presence
    /// that no longer exists cannot leave the selector sitting on a hosted
    /// rung, which would offer to restore something and then not.
    pub level: String,
    /// Whether this call is what moved the identity into its deleted state.
    /// `false` means it was already deleted — a retry, or a second device — and
    /// is a success, not an error.
    pub newly_deleted: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.request_tombstone` request — the durable record of
/// the "also permanently retire this identity" opt-in *inside* the delete
/// ceremony (`atproto-pds-bridge.md` § Disable & revocation layer 2, S5 slice
/// 5b). USER-class and self-scoped, like its sibling.
///
/// It is a separate kind from [`DeletePresenceRequest`] rather than a field on
/// it, for the reason that request's own docs give: nest cannot perform the
/// tombstone, so it must not appear to accept one. What it accepts is the
/// *intent* — and the intent has to be durable here rather than in the client
/// because the act it authorizes spans a crash. The client signs and submits the
/// tombstone on a later converge pass, once the presence sweep has finished; a
/// client that died in between would otherwise leave the user believing their
/// identity was retired when nothing had been published.
///
/// Carries no parameters: the only question the ceremony asks is whether to
/// proceed, and which identity is retired is never the caller's to choose.
/// Idempotent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RequestTombstoneRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.request_tombstone` reply.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RequestTombstoneReply {
    /// Whether this call is what recorded the intent. `false` means it was
    /// already recorded — a retry, or a second device — and is a success.
    pub newly_requested: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.record_tombstone` request — the client reporting that
/// it published the PLC tombstone and the directory accepted it (S5 slice 5b).
/// USER-class, self-scoped.
///
/// Nest records what the client observed and nothing more. It holds no key that
/// could sign the operation and no independent view of the directory, so this
/// reply is testimony, not verification — which is exactly the shape the
/// key-custody split dictates (`atproto-pds-bridge.md` § State & data shape).
/// The client sends it for a log it found *already* tombstoned too, so a crash
/// between submitting and reporting converges instead of leaving the row a state
/// behind the network.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RecordTombstoneRequest {
    /// The operation the tombstone chained to, as the client read it off the
    /// directory's own log. Kept for the audit trail — it is the last thing
    /// anyone can say about an identity that no longer resolves. Empty when the
    /// client found the log already tombstoned and so chained nothing itself.
    #[serde(default)]
    pub prev_cid: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.record_tombstone` reply.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RecordTombstoneReply {
    /// Whether this call is what moved the identity into its retired state.
    /// `false` means it was already recorded.
    pub newly_tombstoned: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.get_integration_status` request (no parameters) —
/// the USER-class read the `atproto` page machine renders from: the caller's
/// current level, the real-domain gate verdict, and their hosted identity
/// summary in one self-scoped fetch (`docs/goal/ui/atproto.md` § State & data
/// shape). Replay-safe read.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GetIntegrationStatusRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The caller's Fauna-hosted ATProto identity, summarized for rendering.
/// Present whenever an identity row exists — active *or* deactivated — so a
/// stepped-down user still sees what re-enabling would restore
/// (`docs/goal/ui/atproto.md` § Errors & edge cases).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AtprotoIdentitySummary {
    /// The ATProto handle, derived at read time from the current Fauna
    /// handle and handle-domain (never stored); empty when the handle does
    /// not derive (reserved label, non-derivable domain).
    pub handle: String,
    /// DID method: `"plc"` or `"web"` — a fact once minted, displayed not
    /// chosen.
    pub method: String,
    /// `"pending"` (mint owed), `"active"`, `"deactivated"` (layer-2
    /// step-down; reversible), `"deleted"` (the presence sweep ran; the DID
    /// and keys are retained, so re-entering a hosted level restores this same
    /// identity), or `"tombstoned"` (the terminal one — the client published a
    /// PLC tombstone, the DID no longer resolves, and no hosted level can be
    /// re-entered on it).
    pub status: String,
    /// The user opted into permanently retiring this identity and the client
    /// has not published the tombstone yet (S5 slice 5b). This is what the
    /// client's converge pass reads to know it still owes the act — and what a
    /// *second* device reads to know a retirement is in flight.
    #[serde(default)]
    pub tombstone_requested: bool,
    /// The minted DID; `None` while `pending`. Rendered nowhere today (the
    /// raw DID string is never shown — § Layout & flow), carried for
    /// diagnostics and the S4-C client-side genesis verification.
    pub did: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.get_integration_status` reply.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GetIntegrationStatusReply {
    /// The persisted level (`"off"` | `"linked"` | `"hosted_visible"` |
    /// `"hosted_full"`).
    pub level: String,
    /// The real-domain gate verdict, computed nest-side with the same
    /// predicate the identities roster applies
    /// (`resolve_handle_domain(domain).is_public_dns_name`): `false` greys
    /// the two hosted rungs client-side (reason shown, never hidden).
    pub hosted_allowed: bool,
    /// The nest's current handle-domain — the value the verdict was computed
    /// from, so the greyed-rung reason line can name it.
    pub handle_domain: String,
    /// The would-be ATProto handle for this caller (`alice.example.com`),
    /// derived at read time — the "your handle is @you.yourdomain either
    /// way" line renders from this before any identity exists. Empty when
    /// the handle does not derive.
    pub handle_preview: String,
    /// The caller's hosted identity, when one exists (any status).
    pub identity: Option<AtprotoIdentitySummary>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── Bridge-class: preferences (app.bsky.actor.{get,put}Preferences) ──

/// Hard cap on a stored preferences payload (`store_preferences`). ATProto
/// preferences are a bounded array (saved feeds, muted words, labeler
/// subscriptions, thread/feed view prefs); 100 KiB is generous headroom for
/// the largest realistic set while bounding a hostile client. A hard-coded
/// constant, never a config knob (the product invariant: clients are the only
/// configuration surface). The Go bridge mirrors this value
/// (`internal/atprotopds`), pre-checking so an oversized `putPreferences` gets
/// a clean XRPC `InvalidRequest` instead of shipping the body to the nest; the
/// nest re-checks here as the source-of-truth authority.
pub const PREFERENCES_MAX_BYTES: usize = 100 * 1024;

/// `fauna.bridges.atproto.fetch_preferences` — the bridge serves
/// `app.bsky.actor.getPreferences` from nest state (D4 custody: preferences
/// are private, and the nest is their source of truth). The blob is **opaque**
/// to the nest — it stores and returns bytes and interprets nothing (D2: no
/// Fauna concept invented).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchPreferencesRequest {
    /// The account whose preferences to read (the authenticated XRPC caller,
    /// resolved bridge-side from the access token).
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchPreferencesReply {
    /// The stored opaque preferences payload, or `None` when the account has
    /// never stored preferences (the bridge then answers an empty array).
    pub preferences: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.store_preferences` — `app.bsky.actor.putPreferences`.
/// Opaque passthrough with a hard size cap ([`PREFERENCES_MAX_BYTES`]); the
/// nest overwrites the account's single preferences row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StorePreferencesRequest {
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// The opaque preferences payload to persist verbatim.
    #[serde(with = "serde_bytes")]
    pub preferences: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StorePreferencesReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── Blobs (F2.4 — `com.atproto.repo.uploadBlob`) ────────────────

/// `fauna.bridges.atproto.record_blob` — tie the ATProto blob CID an external
/// app's record will reference to the Fauna media `ContentHash` the bytes landed
/// under (`atproto-pds-full.md` § F2 detail's `uploadBlob` bullet).
///
/// **Why this is its own call rather than something the byte upload infers.**
/// The bytes reach the nest over the sanctioned bulk-binary carve-out
/// (`POST /api/v1/blob`), which authenticates as the *bridge's* service user and
/// records that actor for audit only — it cannot know which account uploaded.
/// The authenticated `fauna_actor` exists only on the session-token side, so the
/// account attribution rides here, on the one surface that holds it.
///
/// Idempotent by construction: `cid` is the sha256 of the bytes, so a retry
/// after a lost reply carries the identical CID and `media_ref`, and the write
/// upserts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordBlobRequest {
    /// The account that uploaded the blob (the authenticated XRPC caller,
    /// resolved bridge-side from the access token).
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// The ATProto blob CID — CIDv1, raw codec, sha2-256 — as the record's blob
    /// `ref` will spell it, and as `com.atproto.sync.getBlob` is asked for it.
    pub cid: String,
    /// The blake3 `ContentHash` the nest's byte route answered: the name a Fauna
    /// post's media carries for these bytes.
    #[serde(with = "serde_bytes")]
    pub media_ref: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordBlobReply {
    pub ok: bool,
    /// `media_ref` spelled as a canonical Fauna content CID (multibase base32) —
    /// the way a Fauna post's media descriptor names those same bytes.
    ///
    /// **The nest answers it so the bridge never constructs one.** The bridge's
    /// blob store records this string as a blob's provenance, and the projection
    /// loop's "have I already published these bytes?" lookup matches on it, so a
    /// spelling that differs by one character from what the outbound extraction
    /// produces (`ContentHash::to_base32`) would silently re-fetch and re-hash
    /// every image forever, with nothing failing. Deriving it here — where the
    /// `ContentHash` type and its base32 form are owned — makes that drift
    /// unrepresentable rather than merely unlikely.
    pub fauna_cid: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── Authoring keys (D10 delegated authoring) ────────────────────

/// `fauna.bridges.atproto.fetch_authoring_key` — USER-class, self-scoped:
/// the caller fetches their delegated authoring sub-key's public key,
/// minting the sub-key nest-side on the first call (provision-on-read,
/// first-write-wins). The client identity-signs a `DeviceAuthorization` cert
/// over this `k_pub` and uploads it via `provision_authoring_delegation`
/// (`atproto-pds-full.md` D10 § Mint ceremony). The secret half never leaves
/// the nest process.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FetchAuthoringKeyRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchAuthoringKeyReply {
    /// The 32-byte Ed25519 public key of the account's authoring sub-key K.
    #[serde(with = "serde_bytes")]
    pub k_pub: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.provision_authoring_delegation` — USER-class,
/// self-scoped: upload the identity-signed delegation cert authorizing the
/// account's sub-key to author on its behalf. The nest verifies the cert is
/// (a) signed by the caller's identity, (b) grants the caller as author, (c)
/// names the account's own minted `k_pub` as `device_key`, (d) carries only
/// the enumerated authoring capabilities (Post, UpdateProfile), and (e) is
/// not already expired — then stores it (`atproto-pds-full.md` D10).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisionAuthoringDelegationRequest {
    /// The delegation cert as its canonical embed-as-bytes wire (the
    /// identity-signed `DeviceAuthorization`).
    #[serde(with = "serde_bytes")]
    pub cert: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisionAuthoringDelegationReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.revoke_authoring_delegation` — USER-class,
/// self-scoped: destroy the account's authoring sub-key (D10 § Revocation).
/// The row — secret, pubkey, cert — is deleted; already-published posts stay
/// verifiable from their embedded cert. Re-enabling a hosted level re-mints.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RevokeAuthoringDelegationRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokeAuthoringDelegationReply {
    /// `true` iff a sub-key existed and was destroyed (a revoke with no
    /// delegation present is a no-op success — the desired end-state holds).
    pub revoked: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.fetch_authoring_delegation` — USER-class,
/// self-scoped: read back what the account's authoring delegation currently
/// is, so the AT Protocol page can render its status row (D10 § Mint ceremony,
/// "the atproto page's hosted level shows the delegation").
///
/// **A pure read: it mints nothing.** That is the whole reason it is not
/// `fetch_authoring_key` — that kind mints `K` on first call (provision-on-read,
/// first-write-wins), so using it as the page-load status read would mint a
/// sub-key for every user who merely *opens* the page. Here an account that has
/// never run the ceremony reads back two `None`s and no row is created.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FetchAuthoringDelegationRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FetchAuthoringDelegationReply {
    /// The 32-byte Ed25519 public key of the account's authoring sub-key `K`,
    /// or `None` when no sub-key has been minted. Present-but-`cert`-absent is
    /// a real, reachable state: the client fetched `K_pub` and was interrupted
    /// before provisioning (or provisioning was refused). It authorizes
    /// nothing — a sub-key without a cert cannot author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k_pub: Option<ByteBuf>,
    /// The stored delegation cert as its canonical embed-as-bytes wire —
    /// **verbatim the bytes the client uploaded**, not a nest-side summary of
    /// them. The client decodes and re-verifies the envelope under its *own*
    /// identity key, so a nest serving a cert the user never signed is
    /// detectable client-side; and the nest stays a non-interpreter of a
    /// structure it only stores (D2's instinct).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<ByteBuf>,
    /// Epoch-millis of the last external-app write that actually applied under
    /// this delegation, or `None` for "not used yet" (D10 § Audit).
    ///
    /// **Advisory only, and the client must present it as such.** Unlike `cert`
    /// — which the client re-verifies under its own identity key — this is a
    /// nest-asserted number with nothing signing it, so it is *not* proof of
    /// use and emphatically *not* proof of NON-use: a compromised nest can
    /// under-report it at will, and a refused attempt never stamps it. The real
    /// D10 audit surface is the delegated content itself, verified client-side
    /// (the Audit bullet's "the signed bytes are the log"); this only helps an
    /// owner notice a delegation they have forgotten about. Same honest-bound
    /// framing as `ui/nests.md`'s grant row.
    ///
    /// Additive (`default`), so an older nest omitting it reads as `None` and an
    /// older client ignores it — full bidirectional compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── Bridge-class: the F2 external write path ────────────────────

/// The most writes one `ingest_external_write` batch may carry.
///
/// `applyWrites` is one funnel commit (`atproto-pds-full.md` § F2 detail), so
/// a batch is bounded rather than streamed. The ecosystem's own `applyWrites`
/// cap is 200; matching it keeps a legal upstream request from being refused
/// here for a reason the caller cannot see.
pub const EXTERNAL_WRITE_BATCH_MAX: usize = 200;

/// The largest single record the nest accepts, in dag-cbor bytes. The ecosystem
/// caps a repo record at 1 MiB; the bridge mirrors this exact constant and
/// pre-checks, so in practice this is the backstop rather than the first refusal.
///
/// **Corrected 2026-07-29.** This said "the bridge has
/// already validated against the lexicon", which is false and misleading in the
/// same way that produced: **no Lexicon validation exists anywhere** —
/// `validate: true` is answered `MethodNotImplemented`, and the bridge's
/// structural pass is a generic JSON→dag-cbor encode. The *size* half is honestly
/// a backstop because the bridge mirrors the cap; the lexicon half named a layer
/// that has never been built, so a future session would conclude records arrive
/// schema-checked. The nest's own enforcement is therefore load-bearing, not
/// belt-and-braces, and it is decided **before any row of a batch is applied**
/// (`bridge_atproto_handlers::check_write_input_shape`) precisely so the
/// all-or-nothing guarantee does not rest on the bridge having checked first.
pub const EXTERNAL_WRITE_RECORD_MAX_BYTES: usize = 1024 * 1024;

/// Which repo verb produced a write. On the wire as a lowercase string
/// rather than a CBOR enum so an unrecognized value is a *handled* refusal
/// instead of a decode failure (transport.md § Schema and forward-compat
/// discipline).
pub mod external_write_action {
    /// `com.atproto.repo.createRecord` / `applyWrites` `#create`.
    pub const CREATE: &str = "create";
    /// `com.atproto.repo.putRecord` / `applyWrites` `#update`.
    pub const UPDATE: &str = "update";
    /// `com.atproto.repo.deleteRecord` / `applyWrites` `#delete`.
    pub const DELETE: &str = "delete";
}

/// One write inside an [`IngestExternalWriteRequest`] batch.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ExternalWrite {
    /// The record's collection NSID — the input to the D2/D6 membership
    /// classifier (`fauna_bridge_atproto::membership`).
    pub collection: String,
    /// One of [`external_write_action`]'s constants.
    pub action: String,
    /// The record key.
    ///
    /// Required for `update`/`delete` (they address an existing record) and
    /// for a `create` the nest journals. Optional on a round-trip `create`:
    /// the projected rkey is deterministic from the Fauna post's creation
    /// instant (D1), so the nest derives it and the reply reports what it
    /// chose.
    #[serde(default)]
    pub rkey: Option<String>,
    /// The record's dag-cbor bytes, exactly as the bridge validated and
    /// encoded them. Absent on `delete`. Journal rows store these verbatim —
    /// re-derivability depends on the bytes not being re-encoded here.
    #[serde(default)]
    pub record: Option<ByteBuf>,
    /// The record's CID, computed bridge-side by the single indigo encoder
    /// (`dagCBOR.Sum`) — there is deliberately no second Rust dag-cbor/CID
    /// encoder kept byte-identical to it. Absent on `delete`.
    #[serde(default)]
    pub cid: Option<String>,
    /// AT-URI → Fauna post id, for the records this write refers to (F2.2
    /// slice 4b).
    ///
    /// A reply/quote names its target by **AT-URI**, but a Fauna `Reference`
    /// needs the target's **Fauna post id**, and nothing resolves one to the
    /// other nest-side: the projected rkey derivation is one-way and the
    /// reverse index is the bridge's own `post_map`. So the bridge extracts
    /// the record's references (`fauna_bridge_atproto::record_refs`, reached
    /// over FFI), resolves each against `post_map`, and sends the answers
    /// here. On a `delete` this carries the record's own resolution, which is
    /// what the round-trip tombstone arm needs.
    ///
    /// **A map, not named `reply_parent`/`quote` fields**, for two reasons: a
    /// post can be a reply *and* a quote at once, and keeping reference
    /// *kinds* out of the wire is what lets the bridge stay a pure resolver —
    /// a third reference kind later touches neither the bridge nor this type.
    ///
    /// **Absent or unresolved is not an error**: a target that is not a Fauna
    /// post makes the write journal, per the ratified fallback
    /// (`atproto-pds-full.md` § F2 detail). Bounded by
    /// `fauna_bridge_atproto::record_refs::MAX_RECORD_REFS` — deliberately
    /// that one constant rather than a second copy here (this crate sits below
    /// the bridge crate and cannot name it in a doc link), and the nest
    /// re-checks it, since a cap the receiver does not enforce is not a cap.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resolved_targets: BTreeMap<String, String>,
    /// ATProto blob CID → the Fauna `ContentHash` (base32) those bytes are,
    /// for the picture refs this write echoes back at us.
    ///
    /// The second of the two ratified inbound-picture resolvers
    /// (`atproto-pds-full.md` § F2 detail, *a picture crosses INBOUND by
    /// resolution, never by trust*). A **fresh upload** the nest resolves
    /// itself, against the `atproto_blobs` ledger it owns. An **echo** — a
    /// caller handing back the ref our own projection published — it cannot:
    /// the Fauna-CID↔ATProto-CID index is the bridge's blob store, and the two
    /// address spaces are only bridged by holding the bytes. So the bridge
    /// answers about the store it owns, and sends the mapping here.
    ///
    /// **Not trust — resolution.** A hit means this PDS already serves those
    /// bytes for this repo, which is exactly the property *store-then-reference*
    /// demands; a ref neither resolver knows refuses the whole batch rather than
    /// committing a dangling reference. The nest never takes the caller's word
    /// for a picture, only the two stores' answers about themselves.
    ///
    /// **Absent is normal**: today the bridge populates this only for
    /// `app.bsky.actor.profile` writes, the one collection whose refs can be
    /// echoes (a post's images must be uploaded — F2.4 slice 2's ratified
    /// asymmetry). The nest consults the map uniformly all the same: a
    /// resolvable ref is resolvable, and keeping the row-type knowledge on the
    /// producing side is what lets that ruling move without touching the nest.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resolved_media: BTreeMap<String, String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.ingest_external_write` — the whole write batch of
/// one XRPC call, processed in order (`atproto-pds-full.md` § F2 detail).
///
/// Bridge-class: the attested `atproto.pds` host has already authenticated
/// the XRPC session, applied D8 authorization, and structurally validated
/// every record against the lexicon. The nest decides *membership* (D2/D6)
/// and performs the Fauna-side effect.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct IngestExternalWriteRequest {
    /// The account the XRPC session authenticated as.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// The batch, in the order the caller sent it. The reply's results are
    /// positionally aligned with this list.
    pub writes: Vec<ExternalWrite>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Why one write in a batch was refused, sub-typed per D6.
///
/// The sub-type is load-bearing and travels to the XRPC error *name*: a
/// `deferred` refusal must never be read by a later session as permanent
/// policy (`atproto-pds-full.md` § Problem 4).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ExternalWriteRefusal {
    /// `policy` | `fauna_surface` | `deferred` — the serialized spelling of
    /// `fauna_bridge_atproto::membership::RefusalSubType`.
    pub sub_type: String,
    /// Human-readable reason, surfaced in the XRPC error message.
    pub message: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The outcome of one write, positionally aligned with the request batch.
///
/// Exactly one of (`rkey` + `at_uri`) or `refusal` is populated. `cid` is
/// deliberately absent: the bridge fills it from the funnel's own record-CID
/// computation (§ F2 detail, *CID source*).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ExternalWriteResult {
    /// The record key the write landed at. `None` iff refused.
    #[serde(default)]
    pub rkey: Option<String>,
    /// `at://<did>/<collection>/<rkey>`. `None` iff refused.
    #[serde(default)]
    pub at_uri: Option<String>,
    /// The lowercase-hex Fauna post id this write became, when it round-tripped
    /// into a real Fauna post. `None` for a journaled record (which has no
    /// Fauna identity) and for a refusal.
    ///
    /// **The bridge needs this to write its `post_map` row**, and that row is
    /// what makes the write path and the projection loop idempotent with each
    /// other: `Projector.projectPost` skips a post already mapped to an AT-URI,
    /// so without the mapping the loop would re-project this very post and
    /// overwrite the caller's own record bytes with the translator's rendering
    /// of them — handing the caller back a different record than it wrote
    /// (against D1) and emitting a spurious `#commit`.
    #[serde(default)]
    pub fauna_post_id: Option<String>,
    /// Set when the record the repo must carry is **the projection's own
    /// rendering** of the state this write just changed, rather than the
    /// caller's bytes. `false` — the ordinary case — means "commit exactly what
    /// the caller sent".
    ///
    /// True for a collection the projection *owns* and re-derives, today only
    /// the `app.bsky.actor.profile` singleton (F2.3). There the caller's record
    /// is an *input* to a Fauna profile update, not the record that results:
    /// Fauna merges it over the fields ATProto cannot express, and the
    /// projection then re-emits its own rendering of the merged profile. Were
    /// the bridge to commit the caller's bytes, the very next projection pass
    /// would overwrite them — so the synchronously answered `cid` would name
    /// bytes that do not survive, exactly the failure the first-emit ruling
    /// rejects "answer now, commit later" for (§ F2 detail). Rendering through
    /// the projection instead makes `getRecord` agree with the answer, and
    /// makes the next projection pass a no-op by construction (same input →
    /// same record → same CID), which is *why* a profile round-trip needs no
    /// `post_map`-style idempotency row.
    ///
    /// **A flag rather than the record itself (F2.4 slice 3).** Until slice 3
    /// this was `record_override: Option<String>` — the nest rendered the
    /// record JSON and shipped it. That could never be right for an account
    /// with a profile picture: the projected `avatar`/`banner` blob refs carry
    /// the picture's **ATProto** CID, MIME and size from the *bridge's* blob
    /// store (keyed by the picture's Fauna CID), and whether a picture is
    /// publishable at all is **sniffed from its bytes**
    /// (`atproto-pds-bridge.md` § Projection & backfill, which owns that drop
    /// rule). The nest holds neither the index nor the bytes, so a nest-side
    /// rendering would have been a second implementation drifting from the
    /// projection on four axes, three of them silent. So the nest names the
    /// *property* and the bridge renders through the one function a projection
    /// pass uses (`atprotorepo.Projector.RenderProfileRecord`) — equality by
    /// identity instead of by maintenance. Re-add a record-carrying field
    /// additively if a projection-owned collection ever appears that the nest
    /// genuinely can render.
    #[serde(default)]
    pub reproject_record: bool,
    /// Present iff the write was refused; the rest of the batch still ran.
    #[serde(default)]
    pub refusal: Option<ExternalWriteRefusal>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to [`IngestExternalWriteRequest`].
///
/// A refused write does **not** fail the batch: per-write refusals are data,
/// so a mixed `applyWrites` reports precisely which rows the caller must fix.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct IngestExternalWriteReply {
    /// One result per request write, in the same order.
    pub results: Vec<ExternalWriteResult>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── F4 OAuth consent ceremony (D3 rung 2) ───────────────────────

/// One permission set an authorization request named, **as PAR expanded it**
/// (`atproto-pds-full.md:329` — expansion is frozen at the ceremony and never
/// re-resolved, so this is the set's meaning for the grant's whole life).
///
/// It exists because the effective `scopes` list alone cannot say *why* a
/// member is there: a set contributes its members and the bare `include:` scope
/// deliberately does not survive into the token, so without this the card would
/// show a flat list of scopes the user never asked for by name. The set's
/// identity is what makes the expansion legible.
///
/// ⚠ **Every string here except [`nsid`](Self::nsid) is attacker-authored** —
/// they come from a Lexicon record published by whatever DID the NSID's
/// authority names. They cross **raw**: the control-strip fence is the shared
/// machine's, applied in the same composition pass as `client_name`, so all 7
/// apps inherit it at once rather than each painter re-deciding
/// (`atproto-pds-full.md:334`).
///
/// **Ignored members deliberately do not cross.** The expander records each one
/// with a reason (`fauna_bridge_atproto::permission_set::IgnoredMember`) and
/// that is diagnostic data for logs; the card's contract is what the grant
/// *gives*, and adding attacker-authored text about what it does not give would
/// enlarge the surface without giving the user anything to act on.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ConsentSetInfo {
    /// The set's NSID — the identity, rendered **verbatim** by every surface,
    /// exactly as `client_id` is. Nothing derives a display name, an authority
    /// or a fetch target from it here: it was parsed once, before any I/O, by
    /// the component that resolved it.
    pub nsid: String,
    /// The set's human title from its published document, when it declared one.
    #[serde(default)]
    pub title: Option<String>,
    /// The set's human description from its published document, when it
    /// declared one. It never *stands in* for the expansion — see
    /// [`members`](Self::members).
    #[serde(default)]
    pub details: Option<String>,
    /// The expansion: ordinary granular scopes, in document order. These are
    /// also present in the request's effective scope list; carrying them again
    /// under their set is what lets a surface show the grouping without
    /// re-deriving which flat scope came from where.
    pub members: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One pending consent request as an app renders it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PendingConsentRow {
    #[serde(with = "serde_bytes")]
    pub consent_id: Vec<u8>,
    /// The binding code the user compares against their browser.
    pub code: String,
    /// Rendered verbatim — it is the requesting identity, and it is a URL.
    pub client_id: String,
    #[serde(default)]
    pub client_name: Option<String>,
    pub scopes: Vec<String>,
    /// The permission sets behind those scopes, frozen at PAR. Empty for the
    /// ordinary request that named none; additive, so a client older than PS-b
    /// simply ignores it and keeps rendering the flat scope list.
    #[serde(default)]
    pub sets: Vec<ConsentSetInfo>,
    pub created_at: i64,
    pub expires_at: i64,
    /// The X25519 key the ceremony attested as the grant holder
    /// (`fauna_holder_x25519`, `third-party.md` § The principal model rule 2)
    /// — what the approving device wraps the consent-time grant to, and the
    /// key the row the nest mints at `/oauth/token` will carry. Absent for a
    /// client that attested none, to which no grant can be minted. Additive.
    #[serde(default)]
    pub holder_x25519: Option<serde_bytes::ByteBuf>,
    /// The Ed25519 writer key the ceremony attested (`fauna_writer_ed25519`,
    /// `third-party-kinds.md` § Principal write authority) — the key each
    /// `content.write` tuple's factor names. Absent → the principal is
    /// read-only over its kinds, and the card says so. Additive.
    #[serde(default)]
    pub writer_ed25519: Option<serde_bytes::ByteBuf>,
    /// The client document's kind manifest, the compact JWS verbatim
    /// (`third-party-kinds.md` § The manifest) — the nest verified it against
    /// the `client_id`'s host before the request opened, and the approving
    /// device verifies it again before it admits a kind or wraps a key. Absent
    /// for a document without a `fauna` member. Additive.
    #[serde(default)]
    pub fauna_manifest: Option<String>,
    /// Present only on an admin's **install** card (`third-party.md` § The
    /// runner contract → *The install-approval leg*): what approving it would
    /// install and run. Absent on every consent a client started. Additive.
    /// Boxed so the push carrying this row stays under `PushEvent`'s
    /// `large_enum_variant` bound; the wire is unchanged.
    #[serde(default)]
    pub install: Option<Box<ConsentInstallInfo>>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The install section of the one consent card — what `fauna.plugins.install`
/// verified before it opened the row, so the admin approves exactly the
/// bytes the nest will run (`third-party.md` § The runner contract → *The
/// install-approval leg*). Approving grants nothing over any user's data:
/// each user binds the plugin by their own consent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ConsentInstallInfo {
    /// The manifest's `execution.form` — `wasm` today.
    pub execution_form: String,
    /// The `ext.*` kinds the verified manifest declares.
    #[serde(default)]
    pub requested_kinds: Vec<String>,
    /// The scopes the document declares — the ceiling every user's binding
    /// may be granted, never a grant itself.
    #[serde(default)]
    pub requested_scopes: Vec<String>,
    /// The top-level setting names of the manifest's `settings_schema`
    /// (empty when it declares none) — the card's settings summary.
    #[serde(default)]
    pub settings: Vec<String>,
    /// `publisher.domain` — the document's own host.
    pub publisher_domain: String,
    /// `publisher.key`, as the `did:key` the manifest names.
    pub publisher_key: String,
    /// The pinned module digest the fetched bytes matched (`sha256:…`).
    pub module_digest: String,
    /// The hosts the plugin may dial; empty reaches nothing.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// The `ingress` paths the nest would terminate for it.
    #[serde(default)]
    pub ingress_paths: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.list_pending_consents` — USER class, self-scoped: the
/// live consent requests this caller may answer.
///
/// The poll-fallback for the own-device `fauna.atproto.consent_requested` push.
/// An app that was closed when the push fired has no other way to find the card,
/// and the push is best-effort by the same rule every bridge nudge is.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListPendingConsentsRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListPendingConsentsReply {
    /// Oldest first. Includes unassigned requests (a PAR with no `login_hint`),
    /// which any account on this nest may claim by approving.
    pub consents: Vec<PendingConsentRow>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.resolve_consent` — USER class, self-scoped: the
/// approval itself, authenticated by the caller's own Ed25519 identity over
/// normal authed WS-RPC. This is D3 rung 2's whole trust root — the browser
/// session never holds a Fauna secret.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ResolveConsentRequest {
    #[serde(with = "serde_bytes")]
    pub consent_id: Vec<u8>,
    /// `true` approves, `false` declines. A decline is recorded, not silent:
    /// the browser gets a clean refusal instead of a timeout.
    pub approved: bool,
    /// The folder the card's picker chose — the row id of one of the
    /// approving account's folders (`authorization-server.md` § Consent →
    /// *The card chooses the folder*). REQUIRED to approve a row carrying a
    /// bare folder-plane scope, which the nest qualifies with it before the
    /// approval is recorded; refused on an approval of any other row. A
    /// decline carries none. Additive: absent on the wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ResolveConsentReply {
    /// `true` when THIS call is the one that resolved the request. `false`
    /// means there was nothing live to resolve — already answered (possibly on
    /// the user's other device), expired, or not this caller's to answer. The
    /// app re-lists rather than guessing which.
    pub resolved: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.atproto.consent_requested` — own-device fanout to the requesting
/// account's logged-in clients (`atproto-pds-full.md` § WS-RPC kind surface →
/// Push events). Carries everything the approval card renders, so a client that
/// received the push needs no follow-up read.
///
/// Unassigned requests (no `login_hint`) fan out to nobody by construction —
/// there is no account to fan out to — and are found through
/// [`ListPendingConsentsRequest`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AtprotoConsentRequestedPush {
    pub consent: PendingConsentRow,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.permission_set_requested` — the nest→bridge half of
/// the permission-set request call: the nest's `/oauth/par` asks every
/// connected approved `atproto.pds` bridge to resolve one `include:<NSID>`
/// through the bridge's own chain (`atproto-oauth-provider.md`
/// § Implementation status today, the 2026-09-25 bullet).
///
/// **A request, not a nudge.** Unlike every other bridge push this one has no
/// poll-fallback: the bridge answers it directly with
/// [`DeliverPermissionSetRequest`], correlated by `request_id`, and a push
/// that never arrives (an older bridge, a shed frame) is answered by the
/// nest's own deadline — the same `invalid_scope` refusal every other failure
/// gives, never an unverified expansion. It is composed from the two frame
/// directions the transport already has rather than from a nest-initiated
/// `Request` frame, which no client connection may answer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BridgeAtprotoPermissionSetRequestedPush {
    /// 16 random bytes minted per include; the bridge echoes them back.
    #[serde(with = "serde_bytes")]
    pub request_id: Vec<u8>,
    /// The set to resolve — syntax-valid by construction, the only string a
    /// resolution chain may be built from (`permission_set::ParsedInclude`).
    pub nsid: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.deliver_permission_set` — BRIDGE class. The
/// bridge→nest half of the permission-set request call: the answer to one
/// [`BridgeAtprotoPermissionSetRequestedPush`].
///
/// `record` is the resolved set document's dag-cbor **verbatim** — the bytes
/// `atprotolex.SetResolver.ResolveSetDocument` verified against the
/// publisher's MST proof, handed to the shared expander unchanged (a
/// re-encoding would reopen the gap the verification closes). `None` means
/// the chain refused; the reason stays in the bridge's log and is never
/// carried, because it describes the deployment's outbound network to
/// whoever chose the NSID.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DeliverPermissionSetRequest {
    #[serde(with = "serde_bytes")]
    pub request_id: Vec<u8>,
    pub nsid: String,
    #[serde(default)]
    pub record: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to [`DeliverPermissionSetRequest`]. `accepted` is `false` when no
/// request is waiting under that id — the nest's deadline already answered
/// the PAR, or the id was never minted — which is a log line for the bridge
/// and never an error: a late answer is ordinary, not a fault.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DeliverPermissionSetReply {
    pub accepted: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── Push: the bridge nudge ──────────────────────────────────────

/// `fauna.bridges.atproto.sessions_changed` — nudge to every approved
/// `atproto.pds` bridge after a session/credential/kill-switch mutation for
/// an account (nudge + poll-fallback, the `mailbox_state` pattern: the
/// bridge must never *depend* on the push arriving — it re-fetches
/// verifiers/flag on its own schedule too).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BridgeAtprotoSessionsChangedPush {
    /// The account whose external-app state changed.
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    /// `Some(new value)` when the change IS a kill-switch flip — the bridge
    /// updates its cached per-account flag directly from the nudge (no
    /// by-actor fetch kind exists, deliberately). `None` for
    /// session/credential revokes (flag unchanged; the bridge just drops
    /// cached session state for the account).
    #[serde(default)]
    pub external_apps_enabled: Option<bool>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.projection_ready` — nudge to every approved
/// `atproto.pds` bridge that new projection-relevant content landed for a
/// local account: a public post was created, a post was deleted (tombstone
/// journal row written), or a profile was set. The bridge answers by pulling
/// `fauna.bridges.atproto.fetch_public_posts` / `fetch_profile` from its
/// stored cursor (S3, `atproto-pds-bridge.md` § Where logic lives). Nudge +
/// poll-fallback, the `outbound_ready` pattern: best-effort emit, and the
/// bridge's periodic poll is the correctness backstop — it must never
/// *depend* on the push arriving.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BridgeAtprotoProjectionReadyPush {
    /// Optional hint: the account whose projection input changed, so the
    /// bridge can scope its pull to one user instead of sweeping the roster.
    /// `None` means "something changed somewhere" — always safe, since the
    /// poll path sweeps everything anyway.
    #[serde(default)]
    pub actor_id: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.bridges.atproto.issuer_key_rotated` — nudge to every approved
/// `atproto.pds` bridge that the admin rotated the **nest's** OAuth issuer key
/// set (TP5 / S2d leg 1), through either arm. Hint-less: the bridge answers by
/// re-fetching `fauna.bridges.atproto.fetch_issuer_jwks`, which IS the state.
///
/// ⚠ This push is **load-bearing for a compromise response**, not only for
/// availability: the forced rotation arm's window at a verifier is bounded by
/// how fast that verifier re-reads the served set (`authorization-server.md`
/// § The issuer → *Two rotation arms*, "what the forced arm does not bound"). It stays best-effort — the bridge's
/// unknown-`kid` refetch, its reconnect refetch and its ticker are the
/// correctness backstop — but the push is what makes the common case seconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BridgeAtprotoIssuerKeyRotatedPush {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict, encode_canonical};

    /// The transition request round-trips in both shapes it takes: a hosted
    /// entry carrying the mint parameters, and every other transition carrying
    /// none. The mint parameters are flat scalars with defaults rather than a
    /// nested optional struct — dag-cbor cannot round-trip nested `Option`s,
    /// and a growing wire type merges more cleanly flat.
    #[test]
    fn set_integration_level_round_trips_both_shapes() {
        let hosted_entry = SetIntegrationLevelRequest {
            target_level: "hosted_visible".into(),
            did_method: "plc".into(),
            user_rotation_pub_did_key: "did:key:zDnaeTest".into(),
            history_backfill: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&hosted_entry).unwrap();
        let decoded: SetIntegrationLevelRequest = decode_strict(&bytes).unwrap();
        assert_eq!(decoded, hosted_entry);

        let step_down = SetIntegrationLevelRequest {
            target_level: "off".into(),
            ..Default::default()
        };
        let bytes = encode_canonical(&step_down).unwrap();
        let decoded: SetIntegrationLevelRequest = decode_strict(&bytes).unwrap();
        assert_eq!(decoded, step_down);
        assert_eq!(decoded.did_method, "");
        assert!(!decoded.history_backfill);

        let reply = SetIntegrationLevelReply {
            level: "hosted_visible".into(),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SetIntegrationLevelReply = decode_strict(&bytes).unwrap();
        assert_eq!(decoded, reply);
    }

    /// This surface is client↔nest wire (skewable within a major), so unlike
    /// the same-artifact bridge kinds it must ACCEPT unknown fields — the
    /// `extra` flatten, not `deny_unknown_fields`. Pinned here so a future
    /// tidy-up can't quietly restore the stricter (and wire-breaking) posture.
    #[test]
    fn integration_level_wire_accepts_unknown_fields() {
        let mut extra = BTreeMap::new();
        extra.insert("future_field".to_string(), Value::from("future-value"));
        let from_newer_peer = SetIntegrationLevelRequest {
            target_level: "linked".into(),
            extra,
            ..Default::default()
        };
        let bytes = encode_canonical(&from_newer_peer).unwrap();
        let decoded: SetIntegrationLevelRequest = decode_strict(&bytes).unwrap();
        assert_eq!(decoded.target_level, "linked");
        assert_eq!(
            decoded.extra.len(),
            1,
            "unknown field preserved, not refused"
        );
    }

    /// The status read round-trips with and without an identity summary
    /// (single non-nested `Option` — dag-cbor-safe).
    #[test]
    fn get_integration_status_round_trips_both_shapes() {
        let with_identity = GetIntegrationStatusReply {
            level: "hosted_visible".into(),
            hosted_allowed: true,
            handle_domain: "example.com".into(),
            handle_preview: "alice.example.com".into(),
            identity: Some(AtprotoIdentitySummary {
                handle: "alice.example.com".into(),
                method: "plc".into(),
                status: "deactivated".into(),
                did: Some("did:plc:abc123".into()),
                tombstone_requested: false,
                extra: Default::default(),
            }),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&with_identity).unwrap();
        let decoded: GetIntegrationStatusReply = decode_strict(&bytes).unwrap();
        assert_eq!(decoded, with_identity);

        let gated_fresh = GetIntegrationStatusReply {
            level: "off".into(),
            hosted_allowed: false,
            handle_domain: "localhost".into(),
            handle_preview: String::new(),
            identity: None,
            ..Default::default()
        };
        let bytes = encode_canonical(&gated_fresh).unwrap();
        let decoded: GetIntegrationStatusReply = decode_strict(&bytes).unwrap();
        assert_eq!(decoded, gated_fresh);
        assert_eq!(decoded.identity, None);

        let req = GetIntegrationStatusRequest::default();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: GetIntegrationStatusRequest = decode_strict(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn authoring_key_kinds_round_trip() {
        // fetch — request is empty (self-scoped), reply carries the 32-byte pubkey.
        let fetch_req = FetchAuthoringKeyRequest::default();
        let bytes = encode_canonical(&fetch_req).unwrap();
        assert_eq!(
            decode_strict::<FetchAuthoringKeyRequest>(&bytes).unwrap(),
            fetch_req
        );
        let fetch_reply = FetchAuthoringKeyReply {
            k_pub: vec![7u8; 32],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&fetch_reply).unwrap();
        assert_eq!(
            decode_strict::<FetchAuthoringKeyReply>(&bytes).unwrap(),
            fetch_reply
        );

        // provision — the cert rides as opaque embed-as-bytes.
        let prov_req = ProvisionAuthoringDelegationRequest {
            cert: vec![1, 2, 3, 4],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&prov_req).unwrap();
        assert_eq!(
            decode_strict::<ProvisionAuthoringDelegationRequest>(&bytes).unwrap(),
            prov_req
        );
        let prov_reply = ProvisionAuthoringDelegationReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&prov_reply).unwrap();
        assert_eq!(
            decode_strict::<ProvisionAuthoringDelegationReply>(&bytes).unwrap(),
            prov_reply
        );

        // revoke — empty request, `revoked` bool reply.
        let revoke_reply = RevokeAuthoringDelegationReply {
            revoked: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&revoke_reply).unwrap();
        assert_eq!(
            decode_strict::<RevokeAuthoringDelegationReply>(&bytes).unwrap(),
            revoke_reply
        );
    }

    /// The status read's three reachable states all round-trip. The two
    /// `None` arms are the point: an account that never ran the ceremony, and
    /// one interrupted between `fetch_authoring_key` and provisioning, are
    /// both ordinary reads — not errors and not absent replies.
    #[test]
    fn fetch_authoring_delegation_round_trips_all_three_states() {
        let req = FetchAuthoringDelegationRequest::default();
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(
            decode_strict::<FetchAuthoringDelegationRequest>(&bytes).unwrap(),
            req
        );

        for reply in [
            // Never ran the ceremony.
            FetchAuthoringDelegationReply::default(),
            // Fetched K, interrupted before provisioning: a sub-key that
            // authorizes nothing.
            FetchAuthoringDelegationReply {
                k_pub: Some(ByteBuf::from(vec![7u8; 32])),
                cert: None,
                ..Default::default()
            },
            // Fully provisioned, never used by an external app yet.
            FetchAuthoringDelegationReply {
                k_pub: Some(ByteBuf::from(vec![7u8; 32])),
                cert: Some(ByteBuf::from(vec![1, 2, 3, 4])),
                ..Default::default()
            },
            // Fully provisioned AND used — the advisory stamp present.
            FetchAuthoringDelegationReply {
                k_pub: Some(ByteBuf::from(vec![7u8; 32])),
                cert: Some(ByteBuf::from(vec![1, 2, 3, 4])),
                last_used_at: Some(1_780_000_000_000),
                ..Default::default()
            },
        ] {
            let bytes = encode_canonical(&reply).unwrap();
            assert_eq!(
                decode_strict::<FetchAuthoringDelegationReply>(&bytes).unwrap(),
                reply
            );
        }
    }

    #[test]
    fn external_write_batch_round_trips_including_a_refusal_row() {
        let req = IngestExternalWriteRequest {
            actor_id: vec![3u8; 32],
            writes: vec![
                ExternalWrite {
                    collection: "app.bsky.feed.post".into(),
                    action: external_write_action::CREATE.into(),
                    rkey: None,
                    record: Some(ByteBuf::from(vec![0xa1, 0x62, 0x68, 0x69])),
                    cid: Some("bafyexamplecid".into()),
                    // A reply/quote target the bridge resolved. Struct-update
                    // form so the next wire field-add does not break fixtures
                    // — which the `resolved_media` add promptly proved by
                    // breaking this one, because the tail was hand-listed.
                    resolved_targets: [(
                        "at://did:plc:parent/app.bsky.feed.post/3parent".to_string(),
                        "beef".to_string(),
                    )]
                    .into_iter()
                    .collect(),
                    // And a picture ref the bridge resolved against its own
                    // blob store — a map of CID strings like its sibling, so
                    // the two travel identically.
                    resolved_media: [(
                        "bafkreiaha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4".to_string(),
                        "bafyfaunacontenthash".to_string(),
                    )]
                    .into_iter()
                    .collect(),
                    ..Default::default()
                },
                // A delete carries neither record nor cid.
                ExternalWrite {
                    collection: "app.bsky.graph.list".into(),
                    action: external_write_action::DELETE.into(),
                    rkey: Some("3listrkey".into()),
                    record: None,
                    cid: None,
                    ..Default::default()
                },
            ],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(
            decode_strict::<IngestExternalWriteRequest>(&bytes).unwrap(),
            req
        );

        // The reply mixes an applied row with a sub-typed refusal — the shape
        // a partially-refused `applyWrites` produces.
        let reply = IngestExternalWriteReply {
            results: vec![
                ExternalWriteResult {
                    rkey: Some("3listrkey".into()),
                    at_uri: Some("at://did:plc:abc/app.bsky.graph.list/3listrkey".into()),
                    ..Default::default()
                },
                // A round-tripped post additionally carries the Fauna post id
                // the bridge keys its `post_map` row on.
                ExternalWriteResult {
                    rkey: Some("3postrkey".into()),
                    at_uri: Some("at://did:plc:abc/app.bsky.feed.post/3postrkey".into()),
                    fauna_post_id: Some("aa".repeat(32)),
                    ..Default::default()
                },
                // A profile row instead asserts that the repo must carry the
                // PROJECTION's rendering — no record travels on this wire.
                ExternalWriteResult {
                    rkey: Some("self".into()),
                    at_uri: Some("at://did:plc:abc/app.bsky.actor.profile/self".into()),
                    reproject_record: true,
                    ..Default::default()
                },
                ExternalWriteResult {
                    refusal: Some(ExternalWriteRefusal {
                        sub_type: "deferred".into(),
                        message: "posting from external apps is not available yet".into(),
                        extra: Default::default(),
                    }),
                    ..Default::default()
                },
            ],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(
            decode_strict::<IngestExternalWriteReply>(&bytes).unwrap(),
            reply
        );
    }
}
