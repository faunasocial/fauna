//! The shared half of the oracle's two classes the account's delegated
//! authoring sub-key `K` performs — `post.author` (`post_author_oracle.rs`)
//! and `profile.author` (`profile_author_oracle.rs`). Owner:
//! `docs/goal/architecture/key-material-hierarchy.md` § Audience: deployment
//! infrastructure → *The oracle* (the two rows); the key and its delegation:
//! `docs/goal/behavior/atproto-pds-full.md` § D10.
//!
//! One custodian, one key, so one authorization path: the owner's LIVE
//! `identity.op:<class>` grant to the principal's attested key, re-read per
//! request; the delegation present, live by the wall clock and carrying the
//! class's capability; the rate ceiling on the class's own principals table.
//! What differs per class — the request's shape, what is signed, the record —
//! stays in the class's module.

use std::sync::Arc;

use fauna_core::data::Capability;
use fauna_core::encoding::{EmbedAsBytes, canonical_decode};
use fauna_core::identity::ActorKeypair;
use fauna_core::identity_op::{IdentityOpClass, RateWindow};
use fauna_protocol::RpcError;
use rusqlite::{Connection, OptionalExtension, params};

use crate::routes::AppState;
use crate::rpc_errors::internal;

/// How many operation rows each class keeps per principal — the other
/// custodians' constant (`activitypub::oracle::OPS_KEPT_PER_PRINCIPAL`).
pub const OPS_KEPT_PER_PRINCIPAL: i64 = 256;

/// One class this custodian performs, and where its state lives.
#[derive(Debug, Clone)]
pub(crate) struct AuthoringClass {
    /// The class.
    pub class: IdentityOpClass,
    /// The capability the delegation must carry for it.
    pub capability: Capability,
    /// The class's per-principal table: the audit counters and the ceiling's
    /// window (`actor_id`, `principal_id`, `window_start`, `window_count`,
    /// `use_count`, `last_used_at`).
    pub principals_table: &'static str,
    /// The kinds' error family (`fauna.<ns>.permission_denied`).
    pub ns: &'static str,
}

/// Why the custodian refused. The wire answer is deliberately coarse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Any policy or authority check.
    Unauthorized(&'static str),
    /// The rate ceiling.
    RateLimited,
}

impl Refusal {
    /// The wire answer: every policy refusal the same
    /// `fauna.<ns>.permission_denied` — never which check failed; the log
    /// names it — and the ceiling `rate_limited`.
    pub(crate) fn rpc(self, kind: &'static str, ns: &str) -> RpcError {
        match self {
            Refusal::Unauthorized(why) => {
                tracing::info!(target: "permission_gate", kind, why, "oracle refused");
                crate::rpc_errors::permission_denied_ns(ns, "refused")
            }
            Refusal::RateLimited => crate::rpc_errors::rate_limited_ns(ns),
        }
    }
}

/// The owner's live grant of `class` to the principal's attested key.
fn grant_is_live(
    conn: &Connection,
    class: IdentityOpClass,
    account: &[u8; 32],
    holder: &[u8; 32],
    now: u64,
) -> anyhow::Result<bool> {
    let now_i64 = i64::try_from(now).unwrap_or(i64::MAX);
    let mut stmt = conn.prepare_cached(
        "SELECT blob FROM capability_grants
          WHERE owner_actor_id = ?1 AND holder_pubkey = ?2 AND epoch_end > ?3",
    )?;
    let blobs: Vec<Vec<u8>> = stmt
        .query_map(params![&account[..], &holder[..], now_i64], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(blobs
        .iter()
        .any(|b| crate::identity_op_grant::grant_blob_admits(b, class, now)))
}

/// Admit one operation against the principal's window in `table`, storing
/// the advanced position; a refused operation does not consume the window.
fn admit_rate(
    conn: &Connection,
    table: &str,
    account: &[u8; 32],
    principal_id: &[u8],
    now: u64,
) -> anyhow::Result<bool> {
    let actor_hex = hex::encode(account);
    let (window_start, count): (u64, u32) = conn
        .query_row(
            &format!(
                "SELECT window_start, window_count FROM {table}
                  WHERE actor_id = ?1 AND principal_id = ?2"
            ),
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
        return Ok(false);
    };
    conn.execute(
        &format!(
            "INSERT INTO {table} (actor_id, principal_id, window_start, window_count)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (actor_id, principal_id)
             DO UPDATE SET window_start = excluded.window_start,
                           window_count = excluded.window_count"
        ),
        params![actor_hex, principal_id, next.window_start, next.count],
    )?;
    Ok(true)
}

/// Count one performed operation on the principal's row in `table` — the
/// class's module writes its own record row beside it.
pub(crate) fn count_use(
    conn: &Connection,
    table: &str,
    account: &[u8; 32],
    principal_id: &[u8],
    now: u64,
) -> anyhow::Result<()> {
    conn.execute(
        &format!(
            "UPDATE {table} SET use_count = use_count + 1, last_used_at = ?3
              WHERE actor_id = ?1 AND principal_id = ?2"
        ),
        params![hex::encode(account), principal_id, now],
    )?;
    Ok(())
}

/// The grant, the delegation (loaded as the signer), the ceiling — once the
/// class's own shape check passed. `Ok(Err(_))` is a refusal; `Err` is
/// infrastructure.
pub(crate) async fn authorize_and_load(
    state: &Arc<AppState>,
    caller: &crate::principal_handlers::PrincipalCaller,
    class: &AuthoringClass,
    now: u64,
) -> Result<Result<(ActorKeypair, EmbedAsBytes), Refusal>, RpcError> {
    let Some(holder) = caller.holder_x25519 else {
        return Ok(Err(Refusal::Unauthorized("the principal attested no key")));
    };
    // `K` is sealed under the deployment seed the database holds (the mint,
    // `atproto_authoring_key::mint_or_fetch`), read here on the same terms —
    // never a serving generation's copy (`nest_kek`'s module docs).
    let nest_key = {
        let conn = state.db.conn().await;
        if !grant_is_live(&conn, class.class, &caller.account, &holder, now).map_err(internal)? {
            return Ok(Err(Refusal::Unauthorized("no live grant")));
        }
        crate::nest_kek::require_deployment_seed(&conn).map_err(internal)?
    };
    // `load_signer` answers `None` for no key, no cert, or a lapsed cert —
    // the wall-clock expiry point of D10.
    let Some((signer, cert)) =
        crate::atproto_authoring_key::load_signer(&state.db, &nest_key, &caller.account)
            .await
            .map_err(internal)?
    else {
        return Ok(Err(Refusal::Unauthorized("no live authoring delegation")));
    };
    if !crate::atproto_authoring_key::delegation_grants(&cert, &class.capability) {
        return Ok(Err(Refusal::Unauthorized(
            "the delegation does not carry the class's capability",
        )));
    }
    let cert: EmbedAsBytes = canonical_decode(&cert)
        .map_err(|e| internal(format!("stored delegation cert is not embed-as-bytes: {e}")))?;
    let conn = state.db.conn().await;
    if !admit_rate(
        &conn,
        class.principals_table,
        &caller.account,
        &caller.principal_id,
        now,
    )
    .map_err(internal)?
    {
        return Ok(Err(Refusal::RateLimited));
    }
    Ok(Ok((signer, cert)))
}

pub(crate) fn now_secs() -> u64 {
    crate::db::now_epoch_secs().max(0) as u64
}
