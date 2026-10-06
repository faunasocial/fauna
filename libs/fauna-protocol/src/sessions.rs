//! Session-management WS-RPC payload types — `fauna.sessions.{list,revoke,
//! revoke_all,lockout}`. A behavior-preserving transport migration of the
//! bearer-authed session routes (`bins/fauna-nest/src/session_routes.rs`:
//! `list_sessions`, `revoke_session`, `revoke_all_sessions`) plus an authed
//! variant of the emergency lockout. These kinds ride the **bearer**
//! connection (`GET /api/v1/ws/{actor_id}`); the connection authenticates the
//! actor, so — unlike the no-token recovery channel, the pre-identity kind
//! `fauna.account.lockout` (its `POST /api/v1/account/lockout` HTTP twin is
//! deleted — `docs/goal/architecture/api-layers.md` § Sessions) — no
//! signature is carried.
//!
//! **`revoke_all` carries `keep_token_id`** (the short 16-hex token id the
//! caller wants to keep — i.e. its own current session), exactly as `revoke`
//! carries `token_id`. The HTTP twin inferred "the current session" from the
//! request's bearer; the WS connection drops the raw token after the upgrade
//! handshake, so the client names the session to keep instead — it learns its
//! own `token_id` from the `fauna.auth.{handshake,verify}` reply (which now
//! returns it at mint). This mirrors the established client-supplied-identifier
//! convention (push subscriptions carry a client-supplied `device_id`). Track
//! B2 of the WS-RPC-everywhere migration (tracked internally).
//!
//! `token_id` / `keep_token_id` are the short hex display ids (`token_store`'s
//! 8-random-bytes → 16 hex chars), not the secret bearer token. Times are Unix
//! seconds (`u64`); `locked_until` is the DB's `i64`. No floats.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

// ── fauna.sessions.list (≡ GET /api/v1/account/sessions) ────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionsListRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One active session, mirroring `token_store::SessionInfo`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionInfo {
    /// Short 16-hex token id (for display + revocation).
    pub token_id: String,
    /// Unix seconds the token was minted.
    pub created_at: u64,
    /// Unix seconds the token expires.
    pub expires_at: u64,
    /// Creation-time client IP, if recorded.
    pub ip_address: Option<String>,
    /// Unix seconds the token was last validated.
    pub last_used_at: u64,
    /// Hex-encoded renewal device public key, when this session was minted by
    /// a device grant over `fauna.auth.device_handshake` rather than a direct
    /// sign-in (`sync-agent.md` § Credential model — the audit half of device
    /// revocation: without this, the sessions list cannot distinguish an app
    /// sign-in from a device-grant renewal). `None` for every direct-auth
    /// mint. Wire-additive (`#[serde(default)]` — a direct-auth reply simply
    /// lacks it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minted_by_device: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionsListReply {
    pub sessions: Vec<SessionInfo>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sessions.revoke (≡ DELETE /api/v1/account/sessions/{token_id}) ─────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokeRequest {
    /// Short 16-hex token id of the session to revoke (must belong to caller).
    pub token_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokeReply {
    pub ok: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sessions.revoke_all (≡ POST /api/v1/account/sessions/revoke-all) ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokeAllRequest {
    /// Short 16-hex token id to keep (the caller's current session). Any of the
    /// caller's other sessions are revoked. A `keep_token_id` matching none of
    /// the caller's sessions simply revokes them all (harmless; recoverable by
    /// re-auth).
    pub keep_token_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RevokeAllReply {
    pub ok: bool,
    /// Number of sessions revoked.
    pub revoked: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.sessions.lockout (authed variant of POST /api/v1/account/lockout) ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LockoutRequest {
    // No duration field: the lockout window is the nest's hard-coded 24-hour
    // constant. The
    // ignored `duration_secs` left the wire with the compat-remnant sweep
    // (`version-compatibility.md` § Dimension 2, the fourth write-off).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LockoutReply {
    pub ok: bool,
    /// Unix seconds until which auth is blocked (request time + the
    /// hard-coded 24-hour window).
    pub locked_until: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    fn round_trip<T>(v: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let bytes = encode_canonical(v).unwrap();
        let back: T = decode(&bytes).unwrap();
        assert_eq!(v, &back);
    }

    #[test]
    fn list_reply_round_trips() {
        round_trip(&SessionsListReply {
            sessions: vec![
                SessionInfo {
                    token_id: "0011223344556677".into(),
                    created_at: 1_700_000_000,
                    expires_at: 1_700_003_600,
                    ip_address: Some("1.2.3.4".into()),
                    last_used_at: 1_700_000_500,
                    minted_by_device: None,
                    extra: BTreeMap::new(),
                },
                SessionInfo {
                    token_id: "8899aabbccddeeff".into(),
                    created_at: 1_700_000_100,
                    expires_at: 1_700_003_700,
                    ip_address: None,
                    last_used_at: 1_700_000_100,
                    minted_by_device: Some("aa".repeat(32)),
                    extra: BTreeMap::new(),
                },
            ],
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn revoke_round_trips() {
        round_trip(&RevokeRequest {
            token_id: "0011223344556677".into(),
            extra: BTreeMap::new(),
        });
        round_trip(&RevokeReply {
            ok: true,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn revoke_all_round_trips() {
        round_trip(&RevokeAllRequest {
            keep_token_id: "0011223344556677".into(),
            extra: BTreeMap::new(),
        });
        round_trip(&RevokeAllReply {
            ok: true,
            revoked: 3,
            extra: BTreeMap::new(),
        });
    }

    #[test]
    fn lockout_round_trips() {
        round_trip(&LockoutRequest {
            extra: BTreeMap::new(),
        });
        round_trip(&LockoutReply {
            ok: true,
            locked_until: 1_700_090_000,
            extra: BTreeMap::new(),
        });
    }
}
