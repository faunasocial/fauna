//! The oracle's ATProto custodian: a third-party **principal** asks for a
//! short-lived ATProto service JWT signed with the account's repo signing key
//! (TP11).
//!
//! Owner: `docs/goal/architecture/key-material-hierarchy.md` § Audience:
//! deployment infrastructure → *The oracle* (the `atproto.service_auth` row)
//! and *The bridge-held custodian*. The key is NOT nest custody: it rests
//! sealed to the first-party `atproto.pds` bridge, which alone opens it. So
//! the class is performed in two halves with one policy site — this door
//! decides, the bridge mints:
//!
//! 1. an approved `atproto.pds` bridge is connected — decided before any
//!    window is consumed or any push goes out;
//! 2. the request's own shape, and the class's half of the deny list: `lxm`
//!    is never an account-mutation, migration, session-lifecycle or
//!    service-auth-mint method (`fauna_bridge_atproto::authz::
//!    service_auth_lxm_admitted`, a hard constant);
//! 3. the owner's LIVE `identity.op:atproto.service_auth` grant to the
//!    principal's attested key — re-read per request, so a revoke refuses the
//!    very next call;
//! 4. `(aud, lxm)` within the principal's **declared set** — the roster row's
//!    `declared_service_auth`, what the consented manifest's `service_auth`
//!    member said (`third-party.md` § The manifest), matched as exact strings;
//! 5. the account's ACTIVE ATProto DID — the token's `iss`, the one rule
//!    `fetch_app_credential_verifiers`'s `login_did` carries;
//! 6. the rate ceiling (`fauna_core::identity_op::RateWindow`, on the
//!    principal's `atproto_oracle_principals` row);
//! 7. the handoff ([`crate::bridge_round_trip`]): push
//!    `fauna.bridges.atproto.service_auth_requested` — the clamped `exp`
//!    ([`fauna_bridge_atproto::service_auth::clamp_exp`]) — to every
//!    connected approved PDS bridge and wait, at most
//!    [`SERVICE_AUTH_RESOLVE_TIMEOUT`], for its
//!    `fauna.bridges.atproto.deliver_service_auth`;
//! 8. record one `atproto_oracle_ops` row (`<aud> <lxm>`) for the token the
//!    bridge minted.
//!
//! Every refusal — a policy arm, no bridge connected, a shed waiter, the
//! bridge declining to mint, the deadline — answers the same
//! `fauna.atproto.permission_denied` (the ceiling: `rate_limited`), never
//! which check failed and never a token this nest did not receive; the log
//! names it.

use std::sync::Arc;
use std::time::Duration;

use fauna_bridge_atproto::authz::service_auth_lxm_admitted;
use fauna_bridge_atproto::service_auth::clamp_exp;
use fauna_core::identity_op::{IdentityOpClass, RateWindow};
use fauna_protocol::atproto_oracle::{KIND_SERVICE_AUTH, ServiceAuthReply, ServiceAuthRequest};
use fauna_protocol::atproto_pds::BridgeAtprotoServiceAuthRequestedPush;
use fauna_protocol::kind_manifest::ServiceAuthEntry;
use fauna_protocol::{PushEvent, RpcError};
use rusqlite::{Connection, OptionalExtension, params};

use crate::bridge_round_trip::RoundTrips;
use crate::db::bridge_service_users::BridgeRole;
use crate::routes::AppState;
use crate::rpc_errors::{encode_reply, internal, malformed};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// The class this custodian performs.
const CLASS: IdentityOpClass = IdentityOpClass::AtprotoServiceAuth;

/// How many operation rows the custodian keeps per principal — the other
/// custodians' bound (`activitypub::oracle::OPS_KEPT_PER_PRINCIPAL`).
pub const OPS_KEPT_PER_PRINCIPAL: i64 = 256;

/// How long the door waits for the PDS bridge to mint.
///
/// The bridge's work is one nest fetch of the sealed identity blob plus an
/// HPKE open — or neither, from its signer cache — then one ES256K signature:
/// a second on any healthy bridge. Ten seconds covers a cold fetch on a loaded
/// box and still answers a silent bridge well inside the kind's own deadline.
pub const SERVICE_AUTH_RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);

/// What the bridge delivered for one request: the minted token, or `None`
/// when it could not mint.
pub type ServiceAuthDelivery = Option<String>;

/// The in-flight service-auth requests — the shared nest→bridge waiter
/// registry ([`crate::bridge_round_trip`]).
pub type ServiceAuthRequests = RoundTrips<ServiceAuthDelivery>;

/// A fresh, empty registry for [`AppState::atproto_service_auth`].
#[must_use]
pub fn new_requests() -> ServiceAuthRequests {
    RoundTrips::new("service-auth")
}

/// Why the custodian refused. The wire answer is deliberately coarse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Any policy, authority or handoff failure.
    Unauthorized(&'static str),
    /// The rate ceiling.
    RateLimited,
}

impl Refusal {
    fn rpc(self) -> RpcError {
        match self {
            Refusal::Unauthorized(why) => {
                tracing::info!(target: "permission_gate", kind = KIND_SERVICE_AUTH, why, "oracle refused");
                crate::rpc_errors::permission_denied_ns("atproto", "refused")
            }
            Refusal::RateLimited => crate::rpc_errors::rate_limited_ns("atproto"),
        }
    }
}

/// What the door hands the bridge, once every check passed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Admitted {
    did: String,
    aud: String,
    lxm: String,
    exp: i64,
}

/// The decision, and the rate window it advances — steps 2–6 of the module
/// doc, on one connection hold. `Err` is infrastructure only (the database);
/// a refusal is `Ok(Err(_))`.
fn authorize(
    conn: &Connection,
    account: &[u8; 32],
    principal_id: &[u8],
    holder: &[u8; 32],
    req: &ServiceAuthRequest,
    now: u64,
) -> anyhow::Result<Result<Admitted, Refusal>> {
    use Refusal::Unauthorized;

    // 2. The request's own shape, and the deny half.
    if req.aud.is_empty() || req.lxm.is_empty() {
        return Ok(Err(Unauthorized("aud and lxm are both required")));
    }
    if !service_auth_lxm_admitted(&req.lxm) {
        return Ok(Err(Unauthorized("lxm on the class's deny list")));
    }

    // 3. The owner's live grant to the principal's attested key.
    let now_i64 = i64::try_from(now).unwrap_or(i64::MAX);
    let blobs: Vec<Vec<u8>> = {
        let mut stmt = conn.prepare(
            "SELECT blob FROM capability_grants
              WHERE owner_actor_id = ?1 AND holder_pubkey = ?2 AND epoch_end > ?3",
        )?;
        stmt.query_map(params![&account[..], &holder[..], now_i64], |r| r.get(0))?
            .collect::<Result<_, _>>()?
    };
    if !blobs
        .iter()
        .any(|b| crate::identity_op_grant::grant_blob_admits(b, CLASS, now))
    {
        return Ok(Err(Unauthorized("no live grant")));
    }

    // 4. The declared set, read off the roster row now.
    let declared: Option<String> = conn
        .query_row(
            "SELECT declared_service_auth FROM third_party_principals
              WHERE actor_id = ?1 AND principal_id = ?2",
            params![&account[..], principal_id],
            |r| r.get(0),
        )
        .optional()?;
    let declared: Vec<ServiceAuthEntry> = declared
        .and_then(|d| serde_json::from_str(&d).ok())
        .unwrap_or_default();
    if !declared.iter().any(|e| e.admits(&req.aud, &req.lxm)) {
        return Ok(Err(Unauthorized("(aud, lxm) outside the declared set")));
    }

    // 5. The account's active ATProto DID — the token's `iss`.
    let did: Option<String> = conn
        .query_row(
            "SELECT did FROM atproto_identities
              WHERE actor_id = ?1 AND status = 'active' AND did IS NOT NULL",
            params![&account[..]],
            |r| r.get(0),
        )
        .optional()?;
    let Some(did) = did.filter(|d| !d.is_empty()) else {
        return Ok(Err(Unauthorized(
            "the account has no active ATProto identity",
        )));
    };

    // 6. The rate ceiling; a refused operation does not consume the window.
    let actor_hex = hex::encode(account);
    let (window_start, count): (u64, u32) = conn
        .query_row(
            "SELECT window_start, window_count FROM atproto_oracle_principals
              WHERE actor_id = ?1 AND principal_id = ?2",
            params![actor_hex, principal_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .unwrap_or((0, 0));
    let Ok(next) = (RateWindow {
        window_start,
        count,
    })
    .admit(now) else {
        return Ok(Err(Refusal::RateLimited));
    };
    conn.execute(
        "INSERT INTO atproto_oracle_principals (actor_id, principal_id, window_start, window_count)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (actor_id, principal_id)
         DO UPDATE SET window_start = excluded.window_start, window_count = excluded.window_count",
        params![actor_hex, principal_id, next.window_start, next.count],
    )?;

    Ok(Ok(Admitted {
        did,
        aud: req.aud.clone(),
        lxm: req.lxm.clone(),
        exp: clamp_exp(req.exp, now_i64),
    }))
}

/// Record a minted token: the principal's counters and one
/// `atproto_oracle_ops` row, trimming to the newest
/// [`OPS_KEPT_PER_PRINCIPAL`] in the same write.
fn record(
    conn: &Connection,
    account: &[u8; 32],
    principal_id: &[u8],
    admitted: &Admitted,
    now: u64,
) -> anyhow::Result<()> {
    let actor_hex = hex::encode(account);
    conn.execute(
        "UPDATE atproto_oracle_principals SET use_count = use_count + 1, last_used_at = ?3
          WHERE actor_id = ?1 AND principal_id = ?2",
        params![actor_hex, principal_id, now],
    )?;
    conn.execute(
        "INSERT INTO atproto_oracle_ops (actor_id, principal_id, at, class, detail)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            actor_hex,
            principal_id,
            now,
            CLASS.name(),
            format!("{} {}", admitted.aud, admitted.lxm)
        ],
    )?;
    conn.execute(
        "DELETE FROM atproto_oracle_ops
          WHERE actor_id = ?1 AND principal_id = ?2 AND id NOT IN
            (SELECT id FROM atproto_oracle_ops WHERE actor_id = ?1 AND principal_id = ?2
              ORDER BY id DESC LIMIT ?3)",
        params![actor_hex, principal_id, OPS_KEPT_PER_PRINCIPAL],
    )?;
    Ok(())
}

/// The connected approved `atproto.pds` bridges — step 1, decided before any
/// window is consumed or any push goes out.
async fn connected_pds_bridges(state: &AppState) -> anyhow::Result<Vec<[u8; 32]>> {
    Ok(state
        .db
        .list_approved_bridge_service_users()
        .await?
        .into_iter()
        .filter(|b| b.role == BridgeRole::AtprotoPds && state.ws.has_connections(&b.ed25519_pubkey))
        .map(|b| b.ed25519_pubkey)
        .collect())
}

/// Step 7: ask the bridges to mint, and wait for the first answer.
async fn mint_through_bridge(
    state: &AppState,
    bridges: &[[u8; 32]],
    account: &[u8; 32],
    admitted: &Admitted,
    deadline: Duration,
) -> Result<String, Refusal> {
    let registry = &state.atproto_service_auth;
    let (id, receiver) = registry.open();
    for bridge in bridges {
        state.ws.notify_push(
            bridge,
            PushEvent::BridgeAtprotoServiceAuthRequested(BridgeAtprotoServiceAuthRequestedPush {
                request_id: id.to_vec(),
                actor_id: account.to_vec(),
                did: admitted.did.clone(),
                aud: admitted.aud.clone(),
                lxm: admitted.lxm.clone(),
                exp: admitted.exp,
                extra: Default::default(),
            }),
        );
    }
    let outcome = match tokio::time::timeout(deadline, receiver).await {
        Ok(Ok(Some(token))) if !token.is_empty() => Ok(token),
        Ok(Ok(_)) => Err(Refusal::Unauthorized("the bridge could not mint")),
        Ok(Err(_shed)) => Err(Refusal::Unauthorized("waiter shed under load")),
        Err(_elapsed) => Err(Refusal::Unauthorized(
            "no bridge answered within the deadline",
        )),
    };
    // A delivered or shed waiter is already gone; a timed-out one goes here.
    registry.forget(&id);
    outcome
}

/// The whole door, with the bridge deadline as an argument so a test can
/// witness the timeout arm without the production ten seconds.
async fn service_auth(
    state: &Arc<AppState>,
    account: &[u8; 32],
    principal_id: &[u8],
    holder: Option<[u8; 32]>,
    req: &ServiceAuthRequest,
    deadline: Duration,
) -> Result<ServiceAuthReply, RpcError> {
    let Some(holder) = holder else {
        return Err(Refusal::Unauthorized("the principal attested no key").rpc());
    };
    let bridges = connected_pds_bridges(state).await.map_err(internal)?;
    if bridges.is_empty() {
        return Err(Refusal::Unauthorized("no connected atproto.pds bridge").rpc());
    }
    let now = crate::db::now_epoch_secs().max(0) as u64;
    let admitted = {
        let conn = state.db.conn().await;
        match authorize(&conn, account, principal_id, &holder, req, now).map_err(internal)? {
            Ok(a) => a,
            Err(refusal) => return Err(refusal.rpc()),
        }
    };
    let token = mint_through_bridge(state, &bridges, account, &admitted, deadline)
        .await
        .map_err(Refusal::rpc)?;
    {
        let conn = state.db.conn().await;
        record(&conn, account, principal_id, &admitted, now).map_err(internal)?;
    }
    Ok(ServiceAuthReply {
        token,
        extra: Default::default(),
    })
}

/// `fauna.atproto.oracle.service_auth` for a third-party **principal**. The
/// dispatch chokepoint already checked the `ThirdParty` ceiling and that the
/// principal's scopes name an `identity.op` class this custodian performs;
/// the rest is the module doc's order.
pub(crate) fn principal_service_auth_handler() -> crate::principal_handlers::PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            let req: ServiceAuthRequest =
                fauna_protocol::decode_strict(&payload).map_err(malformed)?;
            let reply = service_auth(
                &state,
                &caller.account,
                &caller.principal_id,
                caller.holder_x25519,
                &req,
                SERVICE_AUTH_RESOLVE_TIMEOUT,
            )
            .await?;
            encode_reply(&reply)
        })
    })
}

/// The actor-side half: the kind's wire contract lives in the actor router
/// (one kind, one wire contract), but NO actor class holds it — the matrix
/// arm is `ThirdParty` only — so this runs the central gate and is refused
/// there, always. The kind is served by [`principal_service_auth_handler`].
fn actor_handler() -> RpcHandler {
    Box::new(|state, actor_id, _payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission_default(
                &state,
                &actor_id,
                KIND_SERVICE_AUTH,
            )
            .await?;
            // Unreachable while the matrix arm admits no actor class.
            Err(crate::rpc_errors::central_permission_denied())
        })
    })
}

/// Register the door — the client-side twin is
/// `fauna_protocol::kind::register_atproto_oracle_kinds`: light, replay-safe,
/// a deadline that outlasts [`SERVICE_AUTH_RESOLVE_TIMEOUT`].
pub fn register_atproto_oracle_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        KIND_SERVICE_AUTH,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(15),
            handler: actor_handler(),
        },
    );
}

#[cfg(test)]
mod tests;
