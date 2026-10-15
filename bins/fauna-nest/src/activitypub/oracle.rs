//! The oracle's ActivityPub custodian: a third-party **principal** asks for an
//! HTTP Signature with the account's actor key (TP11).
//!
//! Owner: `docs/goal/architecture/key-material-hierarchy.md` § Audience:
//! deployment infrastructure → *The oracle* (the `activitypub.http_sig` row,
//! the deny list, the audit split). The vocabulary is
//! `fauna_core::identity_op`; the request policy — the target, the method,
//! the sovereign activity shapes — is `fauna_bridge_activitypub::oracle`.
//! This module is the custodian's half, `fauna.activitypub.oracle.http_sig`
//! on a principal session, in this order:
//!
//! 1. the request's own shape (the shared policy's pure checks);
//! 2. the owner's LIVE `identity.op:activitypub.http_sig` grant to the
//!    principal's attested key — re-read per request, so a revoke refuses
//!    the very next call;
//! 3. the target's host within the principal's **declared peer set** — the
//!    roster row's `declared_activitypub_peers`, what the consented manifest's
//!    `activitypub_peers` member said (`third-party.md` § The manifest);
//! 4. the account federates, and the body is one of its actor's own
//!    activities and no sovereign shape;
//! 5. the rate ceiling (`fauna_core::identity_op::RateWindow`, on the
//!    principal's `ap_oracle_principals` row);
//! 6. sign — `date` the nest's clock, `digest` computed from the body —
//!    and record one `ap_oracle_ops` row.
//!
//! Every refusal answers the same `fauna.activitypub.permission_denied` (the
//! ceiling: `rate_limited`) — never which check failed; the log names it.

use fauna_bridge_activitypub::http_signatures::{build_signature_header, compute_digest};
use fauna_bridge_activitypub::oracle::{
    OracleMethod, activity_admitted, check_body_shape, parse_method, parse_target,
};
use fauna_core::identity_op::{IdentityOpClass, RateWindow};
use fauna_protocol::RpcError;
use fauna_protocol::activitypub_oracle::{HttpSigReply, HttpSigRequest, KIND_HTTP_SIG};
use rusqlite::{Connection, OptionalExtension, params};

use crate::rpc_errors::{encode_reply, internal, malformed};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// The class this custodian performs.
const CLASS: IdentityOpClass = IdentityOpClass::ActivityPubHttpSig;

/// How many operation rows the custodian keeps per principal (a hard
/// constant: the record is the owner's recent-activity view, not an archive —
/// the Nostr custodian's `OPS_KEPT_PER_CLIENT`).
pub const OPS_KEPT_PER_PRINCIPAL: i64 = 256;

/// Why the custodian refused. The wire answer is deliberately coarse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Any policy or authority check.
    Unauthorized(&'static str),
    /// The rate ceiling.
    RateLimited,
}

impl Refusal {
    fn rpc(self) -> RpcError {
        match self {
            Refusal::Unauthorized(why) => {
                tracing::info!(target: "permission_gate", kind = KIND_HTTP_SIG, why, "oracle refused");
                crate::rpc_errors::permission_denied_ns("activitypub", "refused")
            }
            Refusal::RateLimited => crate::rpc_errors::rate_limited_ns("activitypub"),
        }
    }
}

/// What the custodian needs to sign, once every check passed.
struct Admitted {
    method: OracleMethod,
    host: String,
    path: String,
    actor_url: String,
    sealed_key: Vec<u8>,
}

/// The decision, and the rate window it advances — steps 2–5 of the module
/// doc, on one connection hold. `Err` is infrastructure only (the
/// database); a refusal is `Ok(Err(_))`.
fn authorize(
    conn: &Connection,
    account: &[u8; 32],
    principal_id: &[u8],
    holder: &[u8; 32],
    req: &HttpSigRequest,
    now: u64,
) -> anyhow::Result<Result<Admitted, Refusal>> {
    use Refusal::Unauthorized;

    // 1. The request's own shape.
    let Some(method) = parse_method(&req.method) else {
        return Ok(Err(Unauthorized("method")));
    };
    let Ok(target) = parse_target(&req.target) else {
        return Ok(Err(Unauthorized("target")));
    };
    let body = req.body.as_deref().map(|b| b.as_slice());
    if check_body_shape(method, body).is_err() {
        return Ok(Err(Unauthorized("body shape")));
    }

    // 2. The owner's live grant to the principal's attested key.
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

    // 3. The declared peer set, read off the roster row now.
    let peers: Option<String> = conn
        .query_row(
            "SELECT declared_activitypub_peers FROM third_party_principals
              WHERE actor_id = ?1 AND principal_id = ?2",
            params![&account[..], principal_id],
            |r| r.get(0),
        )
        .optional()?;
    let peers: Vec<String> = peers
        .and_then(|p| serde_json::from_str(&p).ok())
        .unwrap_or_default();
    if !peers.contains(&target.host) {
        return Ok(Err(Unauthorized("host outside the declared peer set")));
    }

    // 4. The account federates; the body is its actor's own, never sovereign.
    let actor_hex = hex::encode(account);
    let Some(acct) = super::db_helpers::get_account(conn, &actor_hex)? else {
        return Ok(Err(Unauthorized("the account does not federate")));
    };
    if !acct.enabled {
        return Ok(Err(Unauthorized("federation is switched off")));
    }
    if let Some(body) = body
        && activity_admitted(body, &acct.actor_url).is_err()
    {
        return Ok(Err(Unauthorized("activity")));
    }

    // 5. The rate ceiling; a refused operation does not consume the window.
    let (window_start, count): (u64, u32) = conn
        .query_row(
            "SELECT window_start, window_count FROM ap_oracle_principals
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
        "INSERT INTO ap_oracle_principals (actor_id, principal_id, window_start, window_count)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (actor_id, principal_id)
         DO UPDATE SET window_start = excluded.window_start, window_count = excluded.window_count",
        params![actor_hex, principal_id, next.window_start, next.count],
    )?;

    Ok(Ok(Admitted {
        method,
        host: target.host,
        path: target.path,
        actor_url: acct.actor_url,
        sealed_key: acct.encrypted_privkey,
    }))
}

/// Record a performed operation: the principal's counters and one
/// `ap_oracle_ops` row, trimming to the newest [`OPS_KEPT_PER_PRINCIPAL`] in
/// the same write.
fn record(
    conn: &Connection,
    account: &[u8; 32],
    principal_id: &[u8],
    admitted: &Admitted,
    now: u64,
) -> anyhow::Result<()> {
    let actor_hex = hex::encode(account);
    conn.execute(
        "UPDATE ap_oracle_principals SET use_count = use_count + 1, last_used_at = ?3
          WHERE actor_id = ?1 AND principal_id = ?2",
        params![actor_hex, principal_id, now],
    )?;
    conn.execute(
        "INSERT INTO ap_oracle_ops (actor_id, principal_id, at, class, detail)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            actor_hex,
            principal_id,
            now,
            CLASS.name(),
            format!("{} {}", admitted.method.as_str(), admitted.host)
        ],
    )?;
    conn.execute(
        "DELETE FROM ap_oracle_ops
          WHERE actor_id = ?1 AND principal_id = ?2 AND id NOT IN
            (SELECT id FROM ap_oracle_ops WHERE actor_id = ?1 AND principal_id = ?2
              ORDER BY id DESC LIMIT ?3)",
        params![actor_hex, principal_id, OPS_KEPT_PER_PRINCIPAL],
    )?;
    Ok(())
}

/// `fauna.activitypub.oracle.http_sig` for a third-party **principal**. The
/// dispatch chokepoint already checked the `ThirdParty` ceiling and that the
/// principal's scopes name an `identity.op` class this custodian performs;
/// the rest is the module doc's order.
pub(crate) fn principal_http_sig_handler() -> crate::principal_handlers::PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            let req: HttpSigRequest = fauna_protocol::decode_strict(&payload).map_err(malformed)?;
            let Some(holder) = caller.holder_x25519 else {
                return Err(Refusal::Unauthorized("the principal attested no key").rpc());
            };
            let now = crate::db::now_epoch_secs().max(0) as u64;
            let conn = state.db.conn().await;
            let admitted = match authorize(
                &conn,
                &caller.account,
                &caller.principal_id,
                &holder,
                &req,
                now,
            )
            .map_err(internal)?
            {
                Ok(a) => a,
                Err(refusal) => return Err(refusal.rpc()),
            };
            let seed = crate::nest_kek::require_deployment_seed(&conn).map_err(internal)?;
            let privkey_der = super::key_crypto::decrypt_rsa_privkey(&seed, &admitted.sealed_key)
                .map_err(internal)?;
            let date = fauna_core::imf_date::format_http_date(now as i64);
            let digest = req.body.as_deref().map(|b| compute_digest(b.as_slice()));
            let key_id = format!("{}#main-key", admitted.actor_url);
            let signature = build_signature_header(
                &key_id,
                &privkey_der,
                admitted.method.as_str(),
                &admitted.path,
                &admitted.host,
                &date,
                digest.as_deref(),
            )
            .map_err(internal)?;
            record(&conn, &caller.account, &caller.principal_id, &admitted, now)
                .map_err(internal)?;
            drop(conn);
            encode_reply(&HttpSigReply {
                signature,
                key_id,
                host: admitted.host,
                date,
                digest,
                extra: Default::default(),
            })
        })
    })
}

/// The actor-side half: the kind's wire contract lives in the actor router
/// (one kind, one wire contract), but NO actor class holds it — the matrix
/// arm is `ThirdParty` only — so this runs the central gate and is refused
/// there, always. The kind is served by [`principal_http_sig_handler`].
fn actor_handler() -> RpcHandler {
    Box::new(|state, actor_id, _payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission_default(
                &state,
                &actor_id,
                KIND_HTTP_SIG,
            )
            .await?;
            // Unreachable while the matrix arm admits no actor class.
            Err(crate::rpc_errors::central_permission_denied())
        })
    })
}

/// Register the door — the client-side twin is
/// `fauna_protocol::kind::register_activitypub_oracle_kinds`: light,
/// replay-safe.
pub fn register_activitypub_oracle_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        KIND_HTTP_SIG,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: std::time::Duration::from_secs(5),
            handler: actor_handler(),
        },
    );
}
