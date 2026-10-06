//! Account WS-RPC payload types. Today this module carries the single
//! pre-identity kind `fauna.account.register` — a behavior-preserving transport
//! migration of the public `POST /api/v1/register` route
//! (`bins/fauna-nest/src/registration.rs::post_register`); the registration
//! logic lives in the shared `bins/fauna-nest/src/account_core.rs` (the HTTP
//! twin calls the same fn). `register` runs on the **anonymous** WS connection
//! (`GET /api/v1/ws`, no bearer) — the Registration row of the pre-identity
//! allowlist in `docs/goal/architecture/transport.md` § Pre-identity (anonymous)
//! connection. Track A3 of the WS-RPC-everywhere migration (tracked internally).
//!
//! The module is also the home for the **authenticated account surface** —
//! Track B1 of the content umbrella (tracked internally). Those six kinds ride the
//! **bearer** connection (not the pre-identity allowlist) and mirror the
//! existing HTTP routes exactly:
//!
//! - `fauna.account.get` ≡ GET `/api/v1/account` — full account state.
//! - `fauna.quota.get` ≡ GET `/api/v1/quota` — tier-aware usage breakdown.
//! - `fauna.account.am_i_admin` ≡ GET `/api/v1/am-i-admin` — admin-UI gate.
//! - `fauna.profile.handle.change` ≡ PUT `/api/v1/profile/handle` — queue a
//!   handle change as a pending action.
//! - `fauna.account.upgrade` ≡ POST `/api/v1/upgrade` — move to a higher tier.
//! - `fauna.account.delete` ≡ DELETE `/api/v1/account` — queue account deletion.
//!
//! (`/api/v1/export` stays HTTP — a zstd-tar byte download, residue per
//! `api-layers.md` § HTTP residue.)
//!
//! Wire convention (matching `auth.rs` / `discovery.rs`): identity references
//! (`actor_id`) are **hex-encoded `String`**; the dag-cbor wire forbids floats
//! (none here — every numeric field is an `i64`/`bool`) and does not round-trip
//! `Option<Option>` (every optional here is a plain `Option`). The HTTP twin
//! emitted `addresses` conditionally (only when subhandles are enabled); the
//! typed wire carries it uniformly — `addresses: Vec<String>`, empty when
//! subhandles are off — exactly as `discovery::ActorByHandleReply` did with its
//! `addresses`.
//!
//! Kind registry: `kind.rs::register_account_register_kind` (the pre-identity
//! `register`) + `kind.rs::register_account_kinds` (the authenticated surface).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

// ── fauna.account.register (≡ POST /api/v1/register) ────────────────────────

/// Create a new account: claim `handle` for the actor `actor_id`, proving
/// ownership with a domain-tagged signature over
/// [`register_signed_message`]`(actor_id, handle, domain, timestamp)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegisterRequest {
    /// 64-char hex of the registering actor's 32-byte Ed25519 public key.
    pub actor_id: String,
    /// Bare handle (no domain) to claim. Validated per the registration handle
    /// rules; a bad/reserved handle is rejected with `fauna.account.invalid_request`.
    pub handle: String,
    /// Client wall-clock in Unix **milliseconds**; must be within ±30 s of
    /// server time (`fauna.account.invalid_request` otherwise).
    pub timestamp: u64,
    /// 128-char hex of the 64-byte Ed25519 signature over
    /// [`register_signed_message`]`(actor_id, handle, domain, timestamp)` —
    /// the length-prefixed, `ACCOUNT_REGISTER_V1`-tagged element list. The
    /// `domain` is not carried on the wire; the nest recomputes it, trying its
    /// handle domain plus each active mail domain as candidates.
    pub signature: String,
    /// Optional invite code. Present → its tier is granted; absent → the nest's
    /// open/invite-required/free-limit policy applies. `None` (no invite).
    pub invite_code: Option<String>,
    /// The registering app's age claim (`public-mode.md` § Registration Modes
    /// → *Age at registration*; `family-safety.md` § The account age band).
    /// Three consumers, all nest-side: a **minor** claim refuses any path that
    /// would create an *unsupervised* account (`fauna.account.guardian_admission_required`);
    /// a **verified attested** claim satisfies the admin require-knob and
    /// mints the band with `attested-*` provenance; a declared-only claim
    /// mints nothing. Deliberately outside [`register_signed_message`] (shipped
    /// wire — additive evolution): an attested claim is bound to this actor by
    /// the platform's own signature over
    /// [`crate::age::age_claim_signed_message`], which includes `actor_id`.
    /// Additive 2026-08-24.
    #[serde(default)]
    pub age_claim: Option<crate::age::AgeClaim>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The newly-registered account's public coordinates — the body of the
/// `201 Created` the HTTP twin returned.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegisterReply {
    /// 64-char hex of the registered actor's public key (echo of the request).
    pub actor_id: String,
    /// The claimed handle.
    pub handle: String,
    /// The nest's handle domain the account was minted under.
    pub domain: String,
    /// Granted tier (`"free"` unless an invite code upgraded it).
    pub tier: String,
    /// The nest's API base URL (`https://{domain}/api/v1`).
    pub node_url: String,
    /// Subhandle addresses (`handle@domain`, `@handle.domain`); empty when
    /// subhandles are disabled (the HTTP twin's optional `addresses`).
    pub addresses: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The exact bytes a `fauna.account.register` request signs (and the nest
/// verifies): the **length-prefixed** element list
/// `lp(ACCOUNT_REGISTER_V1) ‖ lp(actor_id) ‖ lp(handle) ‖ lp(domain) ‖
/// lp(timestamp_be)` where `lp(x) = (x.len() as u64).to_be_bytes() ‖ x`
/// ([`crate::sig_domain::domain_separated_length_prefixed`]). **Single source of
/// the signed-message contract** — the client signers
/// (`fauna_client_core::auth::build_register_request`,
/// `fauna-onboarding-machine`) and the nest verifier
/// (`account_core::register_core`, which tries each candidate domain) build the
/// message here so they cannot drift.
///
/// Length-prefixing closes item 3 of the finding: the legacy form placed
/// `handle` and `domain` adjacent with no delimiter, so distinct
/// `(handle, domain)` splits of the same byte run signed identically —
/// reachable on a multi-domain nest, whose verifier tries every active domain.
/// The domain tag ([`crate::sig_domain::ACCOUNT_REGISTER_V1`]) is the rule-#8
/// cross-context separation. Crypto-free byte assembly, as with every builder
/// here.
pub fn register_signed_message(
    actor_id: &[u8; 32],
    handle: &str,
    domain: &str,
    timestamp_ms: u64,
) -> Vec<u8> {
    crate::sig_domain::domain_separated_length_prefixed(
        crate::sig_domain::ACCOUNT_REGISTER_V1,
        &[
            actor_id,
            handle.as_bytes(),
            domain.as_bytes(),
            &timestamp_ms.to_be_bytes(),
        ],
    )
}

// ── fauna.account.lockout (≡ POST /api/v1/account/lockout) ──────────────────

/// Emergency **no-token account lockout** — the recovery channel that must work
/// when the actor cannot open an authed WS (e.g. a stolen/compromised device).
/// It authenticates by a domain-tagged Ed25519 signature over
/// [`account_lockout_signed_message`], **not a bearer**, so — like
/// `fauna.account.register` — it rides
/// the **anonymous (pre-identity) WS connection** (`GET /api/v1/ws`), not the
/// per-actor bearer socket. The authed sibling is `fauna.sessions.lockout`
/// (no signature; the connection authenticates the actor). On success the nest
/// revokes every token for the actor and blocks all auth for the hard-coded
/// 24-hour window (`devices.md` § Emergency lockout).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountLockoutRequest {
    /// 64-char hex of the actor's 32-byte Ed25519 public key (the actor id).
    pub actor_id: String,
    /// Client wall-clock in Unix **seconds**; must be within ±300 s of server
    /// time (`fauna.account.invalid_request` otherwise).
    pub timestamp: u64,
    /// 128-char hex of the 64-byte **domain-separated** Ed25519 signature over
    /// [`account_lockout_signed_message`]`(actor_id, timestamp)` =
    /// `b"fauna.account.lockout.v1\0" ‖ actor_id ‖ timestamp_be`
    /// (`fauna_protocol::sig_domain::ACCOUNT_LOCKOUT_V1`). The tag makes a
    /// lockout signature structurally un-confusable with a claim or a login one
    /// — the rule #8 guarantee. The untagged legacy
    /// form was deleted outright under the 2026-08-17 no-existing-users
    /// ratification; nothing else verifies.
    pub signature: String,
    // No duration field: the lockout window is the nest's hard-coded 24-hour
    // constant. The
    // unsigned, ignored `duration_secs` left the wire with the compat-remnant
    // sweep (`version-compatibility.md` § Dimension 2, the fourth write-off);
    // any stray key lands in `extra`.
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The exact bytes a domain-separated `fauna.account.lockout` request signs (and
/// the nest verifies): `ACCOUNT_LOCKOUT_V1 ‖ actor_id ‖ timestamp_be` (timestamp
/// in **seconds**, matching the lockout freshness window). **Single source of the
/// tagged-message contract** — any recovery-client signer and the nest verifier
/// (`bins/fauna-nest/src/routes.rs::verify_account_lockout_signature`) build the
/// message here so they cannot drift. Crypto-free byte assembly (the lean default
/// protocol build stays crypto-free); the domain tag
/// ([`crate::sig_domain::ACCOUNT_LOCKOUT_V1`]) is what separates this context from
/// the byte-identical claim-admin / login messages.
pub fn account_lockout_signed_message(actor_id: &[u8; 32], timestamp: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(40);
    body.extend_from_slice(actor_id);
    body.extend_from_slice(&timestamp.to_be_bytes());
    crate::sig_domain::domain_separated(crate::sig_domain::ACCOUNT_LOCKOUT_V1, &body)
}

/// Catalog-aligned fixture default so construction sites grow new fields via
/// `..Default::default()` rather than hand-listing — the fixture-shape-conflict
/// discipline (two branches independently growing this wire type merge cleanly).
impl Default for AccountLockoutRequest {
    fn default() -> Self {
        Self {
            actor_id: String::new(),
            timestamp: 0,
            signature: String::new(),
            extra: BTreeMap::new(),
        }
    }
}

/// The applied lockout window — the body the HTTP twin returned as
/// `{ "ok": true, "locked_until": … }`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountLockoutReply {
    pub ok: bool,
    /// Unix **seconds** until which auth is blocked (request time + the
    /// hard-coded 24-hour window).
    pub locked_until: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Authenticated account surface (bearer connection) — Track B1.
// ─────────────────────────────────────────────────────────────────────────────

/// A `{used_bytes, max_bytes}` usage pair — the inbox / storage quota cells
/// shared by `AccountGetReply.quota` and `QuotaGetReply`. Mirrors the HTTP
/// twins' nested json objects.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageBytes {
    pub used_bytes: i64,
    pub max_bytes: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.account.get (≡ GET /api/v1/account) ───────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountGetRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Eviction state — present (the HTTP twin's non-null `eviction` object) only
/// when the account is under eviction (`UserRow.eviction_status` non-empty).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountEviction {
    pub status: String,
    pub reason: String,
    pub category: String,
    /// Micros since epoch; absent when not yet set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warned_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspend_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_at: Option<i64>,
    /// One-time data-export token minted when an account is evicted, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_token: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The account-state quota block — mirrors the HTTP twin's `quota` object.
/// Note `devices` here carries only `max` (the twin omitted device usage on
/// this endpoint; `fauna.quota.get` carries `used` too).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountGetQuota {
    pub inbox: UsageBytes,
    pub storage: UsageBytes,
    pub devices: AccountDeviceLimit,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountDeviceLimit {
    pub max: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Node-wide eviction policy (days), echoed so the client can render the
/// eviction-warning UI without a separate node-info round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountNodePolicy {
    pub eviction_warning_days: i64,
    pub eviction_suspension_days: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Full account state — the body of `GET /api/v1/account` (the transparency
/// endpoint). The biggest reply of the slice.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountGetReply {
    /// 64-char hex of the calling actor's public key.
    pub actor_id: String,
    /// The actor's current handle, when one is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    pub tier: String,
    /// Account creation timestamp (micros since epoch).
    pub created_at: i64,
    /// Present only when the account is under eviction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eviction: Option<AccountEviction>,
    pub quota: AccountGetQuota,
    pub node_policy: AccountNodePolicy,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.quota.get (≡ GET /api/v1/quota) ────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuotaGetRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Device usage — `used` (the live count) + `max` (the tier cap). Distinct
/// from `AccountDeviceLimit` (which carries only `max`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuotaDeviceUsage {
    pub used: i64,
    pub max: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Tier-derived feature flags — mirrors the HTTP twin's `features` object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuotaFeatures {
    pub versioned_backup: bool,
    pub bridges: bool,
    pub max_feeds: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Tier-aware usage breakdown — the body of `GET /api/v1/quota`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuotaGetReply {
    pub tier: String,
    pub inbox: UsageBytes,
    pub storage: UsageBytes,
    pub devices: QuotaDeviceUsage,
    pub features: QuotaFeatures,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.account.am_i_admin (≡ GET /api/v1/am-i-admin) ──────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AmIAdminRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AmIAdminReply {
    /// Whether the calling actor is a nest admin — the HTTP twin's `{admin}`.
    pub admin: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.profile.handle.change (≡ PUT /api/v1/profile/handle) ───────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChangeHandleRequest {
    /// The bare handle (no domain) to change to. Validated per the registration
    /// handle rules; rejected with `fauna.profile.invalid_request` if bad/reserved,
    /// `fauna.profile.handle_taken` / `fauna.profile.handle_cooldown` on conflict.
    pub handle: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The queued handle-change pending action (the HTTP twin's `202 Accepted`
/// body). The change is delayed + cancellable (api-layers.md § Destructive
/// operations are delayed); the client polls `pending-actions` for status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChangeHandleReply {
    pub pending_action_id: i64,
    /// Earliest execution timestamp (micros since epoch).
    pub execute_after: i64,
    /// Always `"pending"` on creation.
    pub status: String,
    pub new_handle: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.account.upgrade (≡ POST /api/v1/upgrade) ───────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpgradeRequest {
    /// Target tier (`personal` / `community`); must be strictly higher than the
    /// current tier, else `fauna.account.invalid_request`.
    pub tier: String,
    /// Invite code authorizing the target tier; its tier must match `tier`.
    pub invite_code: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpgradeReply {
    /// Always `true` on success — the HTTP twin's `{ok}`.
    pub ok: bool,
    /// The granted (now-current) tier.
    pub tier: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.account.delete (≡ DELETE /api/v1/account) ──────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountDeleteRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The queued account-deletion pending action (the HTTP twin's `202 Accepted`
/// body). Deletion sits in the queue for the cancellation window (api-layers.md
/// § Destructive operations are delayed) before executing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountDeleteReply {
    pub pending_action_id: i64,
    /// Earliest execution timestamp (micros since epoch).
    pub execute_after: i64,
    /// Always `"pending"` on creation.
    pub status: String,
    /// Human-readable confirmation line (the HTTP twin's `message`).
    pub message: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::test_support::assert_round_trips;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn register_request_round_trips_with_and_without_invite() {
        let with = RegisterRequest {
            actor_id: "ab".repeat(32),
            handle: "alice".into(),
            timestamp: 1_700_000_000_000,
            signature: "cd".repeat(64),
            invite_code: Some("WELCOME2026".into()),
            // Declared-only age claim (additive 2026-08-24) — proves the
            // optional claim rides the wire beside the invite code.
            age_claim: Some(crate::age::AgeClaim {
                band: "13-15".into(),
                attestation: None,
                extra: BTreeMap::new(),
            }),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&with).unwrap();
        assert_eq!(with, decode::<RegisterRequest>(&bytes).unwrap());

        // `None` invite_code → null on the wire → round-trips back to None
        // (plain Option, not Option<Option>).
        let without = RegisterRequest {
            invite_code: None,
            ..with
        };
        let bytes = encode_canonical(&without).unwrap();
        let decoded: RegisterRequest = decode(&bytes).unwrap();
        assert_eq!(without, decoded);
        assert!(decoded.invite_code.is_none());
    }

    #[test]
    fn register_reply_round_trips_with_and_without_addresses() {
        let with = RegisterReply {
            actor_id: "ab".repeat(32),
            handle: "alice".into(),
            domain: "nest.example".into(),
            tier: "free".into(),
            node_url: "https://nest.example/api/v1".into(),
            addresses: vec!["alice@nest.example".into(), "@alice.nest.example".into()],
            extra: BTreeMap::new(),
        };
        let bytes1 = encode_canonical(&with).unwrap();
        let decoded: RegisterReply = decode(&bytes1).unwrap();
        assert_eq!(with, decoded);
        // Canonical re-encode is stable.
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);

        let without = RegisterReply {
            addresses: vec![],
            ..with
        };
        let bytes = encode_canonical(&without).unwrap();
        let decoded: RegisterReply = decode(&bytes).unwrap();
        assert_eq!(without, decoded);
        assert!(decoded.addresses.is_empty());
    }

    #[test]
    fn account_lockout_round_trips() {
        let req = AccountLockoutRequest {
            actor_id: "ab".repeat(32),
            timestamp: 1_700_000_000,
            signature: "cd".repeat(64),
            ..Default::default()
        };
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: AccountLockoutRequest = decode(&bytes1).unwrap();
        assert_eq!(req, decoded);
        // Canonical re-encode is stable.
        assert_eq!(bytes1, encode_canonical(&decoded).unwrap());

        let reply = AccountLockoutReply {
            ok: true,
            locked_until: 1_700_090_000,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        assert_eq!(reply, decode::<AccountLockoutReply>(&bytes).unwrap());
    }

    #[test]
    fn register_signed_message_is_tagged_and_length_prefixed() {
        let actor = [0xab_u8; 32];
        let ts: u64 = 1_700_000_000_000;
        let m = register_signed_message(&actor, "alice", "nest.example", ts);
        // Tag first, itself length-prefixed (the injective form).
        let tag = crate::sig_domain::ACCOUNT_REGISTER_V1;
        assert_eq!(&m[..8], &(tag.len() as u64).to_be_bytes());
        assert_eq!(&m[8..8 + tag.len()], tag);
        // The item-3 resplit is structurally impossible: moving bytes between
        // adjacent variable-length fields changes the signed message.
        assert_ne!(
            register_signed_message(&actor, "alice", "nest.example", ts),
            register_signed_message(&actor, "alicen", "est.example", ts),
        );
        // And the tag separates it from every other actor-key context.
        assert_ne!(m, crate::claim::claim_admin_signed_message(&actor, ts));
    }

    #[test]
    fn account_lockout_signed_message_is_domain_tagged() {
        let actor = [0xcd_u8; 32];
        let ts: u64 = 1_700_000_000;
        let m = account_lockout_signed_message(&actor, ts);
        assert!(m.starts_with(crate::sig_domain::ACCOUNT_LOCKOUT_V1));
        // Distinct from a claim-admin message over the SAME actor+ts — the two
        // actor-key contexts can never be confused once tagged.
        assert_ne!(m, crate::claim::claim_admin_signed_message(&actor, ts));
    }

    // ── Authenticated account surface (Track B1) ────────────────────────────

    fn sample_account_get_reply(evicted: bool) -> AccountGetReply {
        AccountGetReply {
            actor_id: "ab".repeat(32),
            handle: Some("alice".into()),
            tier: "free".into(),
            created_at: 1_700_000_000_000_000,
            eviction: evicted.then(|| AccountEviction {
                status: "warned".into(),
                reason: "over quota".into(),
                category: "storage".into(),
                warned_at: Some(1_700_000_100_000_000),
                suspend_at: Some(1_700_000_200_000_000),
                delete_at: None,
                export_token: Some("tok-123".into()),
                extra: BTreeMap::new(),
            }),
            quota: AccountGetQuota {
                inbox: UsageBytes {
                    used_bytes: 10,
                    max_bytes: 104857600,
                    extra: BTreeMap::new(),
                },
                storage: UsageBytes {
                    used_bytes: 20,
                    max_bytes: 104857600,
                    extra: BTreeMap::new(),
                },
                devices: AccountDeviceLimit {
                    max: 2,
                    extra: BTreeMap::new(),
                },
                extra: BTreeMap::new(),
            },
            node_policy: AccountNodePolicy {
                eviction_warning_days: 7,
                eviction_suspension_days: 14,
                extra: BTreeMap::new(),
            },
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn account_get_request_round_trips() {
        assert_round_trips(&AccountGetRequest {
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn account_get_reply_round_trips_with_eviction() {
        assert_round_trips(&sample_account_get_reply(true));
    }

    #[test]
    fn account_get_reply_omits_eviction_and_handle_when_absent() {
        let reply = AccountGetReply {
            handle: None,
            ..sample_account_get_reply(false)
        };
        assert_round_trips(&reply);
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: AccountGetReply = decode(&bytes).unwrap();
        assert!(decoded.eviction.is_none());
        assert!(decoded.handle.is_none());
    }

    #[test]
    fn quota_get_round_trips() {
        assert_round_trips(&QuotaGetRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&QuotaGetReply {
            tier: "personal".into(),
            inbox: UsageBytes {
                used_bytes: 5,
                max_bytes: 100,
                extra: BTreeMap::new(),
            },
            storage: UsageBytes {
                used_bytes: 6,
                max_bytes: 200,
                extra: BTreeMap::new(),
            },
            devices: QuotaDeviceUsage {
                used: 1,
                max: 5,
                extra: BTreeMap::new(),
            },
            features: QuotaFeatures {
                versioned_backup: true,
                bridges: true,
                max_feeds: 10,
                extra: BTreeMap::new(),
            },
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn am_i_admin_round_trips() {
        assert_round_trips(&AmIAdminRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AmIAdminReply {
            admin: true,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AmIAdminReply {
            admin: false,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn change_handle_round_trips() {
        assert_round_trips(&ChangeHandleRequest {
            handle: "bob".into(),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&ChangeHandleReply {
            pending_action_id: 42,
            execute_after: 1_700_000_000_000_000,
            status: "pending".into(),
            new_handle: "bob".into(),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn upgrade_round_trips() {
        assert_round_trips(&UpgradeRequest {
            tier: "community".into(),
            invite_code: "WELCOME2026".into(),
            extra: BTreeMap::new(),
        });
        assert_round_trips(&UpgradeReply {
            ok: true,
            tier: "community".into(),
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn account_delete_round_trips() {
        assert_round_trips(&AccountDeleteRequest {
            extra: BTreeMap::new(),
        });
        assert_round_trips(&AccountDeleteReply {
            pending_action_id: 7,
            execute_after: 1_700_001_000_000_000,
            status: "pending".into(),
            message: "account deletion scheduled".into(),
            extra: BTreeMap::new(),
        });
    }
}
