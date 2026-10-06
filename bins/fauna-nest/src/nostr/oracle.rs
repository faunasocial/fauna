//! The oracle's Nostr custodian: a third-party **principal** as a NIP-46
//! client of the account's own bunker signer (TP11).
//!
//! Owners: `docs/goal/architecture/key-material-hierarchy.md` § Audience:
//! deployment infrastructure → *The oracle* (the class, the deny list, the
//! audit split) and `docs/goal/ui/nostr.md` § The nest as the user's NIP-46
//! signer → *A principal as a bunker client*. The vocabulary is
//! `fauna_core::identity_op`; the method table and the kind policy are
//! `fauna_bridge_nostr::nip46`. This module is the custodian's half:
//!
//! * [`bind`] — the client row `fauna.nostr.bunker.bind` writes: which NIP-46
//!   client key speaks for which principal. The row is never the authority.
//! * [`authorize`] — run by `bunker::handle_request` for every request whose
//!   author is no invite-roster app. It re-resolves, **per request**, the
//!   principal row and the owner's live `identity.op` grant to the
//!   principal's attested key, so revoking either refuses the very next call.
//!   Then the class-specific policy (the sign_event kind set) and the rate
//!   ceiling. Every refusal answers the same `unauthorized` (the ceiling:
//!   `rate-limited`) — never which check failed.
//! * [`record`] — the per-operation record the custodian keeps: one
//!   `nostr_oracle_ops` row per key operation, newest [`OPS_KEPT_PER_CLIENT`]
//!   per client. The grant's own lifecycle is the client-signed `GrantEvent`
//!   log; the nest holds no owner key, so it never writes one.

use anyhow::{Result, bail};
use fauna_bridge_nostr::nip46::{Nip46Method, Nip46Request, oracle_sign_event_kind_admitted};
use fauna_core::identity_op::{
    Custodian, GrantTupleRef, IdentityOpClass, RateWindow, grant_admits,
};
use fauna_mls::wrapped_blob::format::GrantBlob;
use rusqlite::{Connection, OptionalExtension, params};

/// How many operation rows the custodian keeps per bound client (a hard
/// constant: the record is the owner's recent-activity view, not an archive).
pub const OPS_KEPT_PER_CLIENT: i64 = 256;

/// The custodian this module is.
const CUSTODIAN: Custodian = Custodian::NostrDepositedKey;

/// A request the oracle admitted: the client row to record against and the
/// class the method is performed under (`None` for the key-less methods —
/// `connect`, `ping`, `get_public_key`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OracleAdmission {
    pub client_id: i64,
    pub class: Option<IdentityOpClass>,
}

/// Why the oracle refused a request. The wire answer is deliberately coarse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleRefusal {
    /// No bound client, a gone principal, no live grant naming the class, a
    /// method never served to a principal, or a denied event kind.
    Unauthorized,
    /// The rate ceiling (`fauna_core::identity_op::OPS_PER_WINDOW`).
    RateLimited,
}

impl OracleRefusal {
    /// The NIP-46 `error` string the response carries.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            OracleRefusal::Unauthorized => "unauthorized",
            OracleRefusal::RateLimited => "rate-limited",
        }
    }
}

/// Is `s` a 64-character lowercase-hex Nostr public key?
#[must_use]
pub fn is_client_pubkey(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Does the stored grant `blob` admit `class` at `now`: a decodable grant
/// whose window is live and whose declared scope carries the class's
/// `identity.op` tuple? An undecodable blob admits nothing.
#[must_use]
pub fn grant_blob_admits(blob: &[u8], class: IdentityOpClass, now: u64) -> bool {
    let Ok(grant) = GrantBlob::from_canonical_bytes(blob) else {
        return false;
    };
    // Both bounds, through the one window check every authorizing decode
    // names (`state.rs`'s census).
    i64::try_from(now).is_ok_and(|now| fauna_mls::wrapped_blob::grant_window_is_open(&grant, now))
        && grant_admits(
            grant.scope.iter().map(|t| GrantTupleRef {
                class: t.class.as_str(),
                kind: t.kind.as_deref(),
            }),
            class,
        )
}

/// Does any of `blobs` admit a class THIS custodian performs? The bind
/// handler's precondition: a principal cannot park a client key before the
/// owner granted it anything here.
#[must_use]
pub fn any_grant_admits_this_custodian<'a>(
    blobs: impl IntoIterator<Item = &'a [u8]>,
    now: u64,
) -> bool {
    let classes: Vec<IdentityOpClass> = IdentityOpClass::ALL
        .iter()
        .copied()
        .filter(|c| c.custodian() == CUSTODIAN)
        .collect();
    blobs
        .into_iter()
        .any(|b| classes.iter().any(|c| grant_blob_admits(b, *c, now)))
}

/// Why [`bind`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindRefusal {
    /// Another principal of the same account already bound this client key.
    /// One key, one principal: the per-request lookup is by key, so a second
    /// binding would let one principal's requests resolve another's grants.
    KeyBoundByAnotherPrincipal,
}

/// Write the principal's client row: insert, or re-point the principal's one
/// row at `client_pubkey` (a re-bind). The caller has validated the key's
/// shape and the principal's grant.
pub fn bind(
    conn: &Connection,
    actor_hex: &str,
    principal_id: &[u8],
    client_pubkey: &str,
    now: u64,
) -> Result<std::result::Result<(), BindRefusal>> {
    let other: Option<i64> = conn
        .query_row(
            "SELECT id FROM nostr_oracle_clients
              WHERE actor_id = ?1 AND client_pubkey = ?2 AND principal_id != ?3",
            params![actor_hex, client_pubkey, principal_id],
            |r| r.get(0),
        )
        .optional()?;
    if other.is_some() {
        return Ok(Err(BindRefusal::KeyBoundByAnotherPrincipal));
    }
    conn.execute(
        "INSERT INTO nostr_oracle_clients (actor_id, principal_id, client_pubkey, created_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (actor_id, principal_id) DO UPDATE SET client_pubkey = excluded.client_pubkey",
        params![actor_hex, principal_id, client_pubkey, now],
    )?;
    Ok(Ok(()))
}

/// The per-request decision for a request authored by `app_pubkey` at the
/// signer of `actor_hex` — the module doc's order. `Err` is infrastructure
/// only (the database); a refusal is `Ok(Err(_))`.
///
/// Admitting a class operation advances the client's rate window in the same
/// call, so the ceiling counts operations admitted, whatever their outcome.
pub fn authorize(
    conn: &Connection,
    actor_hex: &str,
    app_pubkey: &str,
    req: &Nip46Request,
    now: u64,
) -> Result<std::result::Result<OracleAdmission, OracleRefusal>> {
    use OracleRefusal::{RateLimited, Unauthorized};

    let client: Option<(i64, Vec<u8>, u64, u32)> = conn
        .query_row(
            "SELECT id, principal_id, window_start, window_count FROM nostr_oracle_clients
              WHERE actor_id = ?1 AND client_pubkey = ?2",
            params![actor_hex, app_pubkey],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((client_id, principal_id, window_start, window_count)) = client else {
        return Ok(Err(Unauthorized));
    };
    let Ok(owner) = hex::decode(actor_hex) else {
        return Ok(Err(Unauthorized));
    };

    // The principal row, read now: a revoked principal is refused at once
    // (its client row is deleted with it, but the read does not rely on that).
    let principal: Option<Option<Vec<u8>>> = conn
        .query_row(
            "SELECT holder_x25519 FROM third_party_principals
              WHERE actor_id = ?1 AND principal_id = ?2",
            params![owner, principal_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(holder) = principal else {
        return Ok(Err(Unauthorized));
    };

    let class = match req.method.oracle_class() {
        Err(_) => return Ok(Err(Unauthorized)),
        Ok(None) => {
            return Ok(Ok(OracleAdmission {
                client_id,
                class: None,
            }));
        }
        Ok(Some(class)) => class,
    };
    if class.custodian() != CUSTODIAN {
        return Ok(Err(Unauthorized));
    }

    // The owner's live grant to the principal's attested key, re-resolved
    // per request — `fauna.capabilities.revoke` severs at the next call.
    let Some(holder) = holder else {
        return Ok(Err(Unauthorized));
    };
    let now_i64 = i64::try_from(now).unwrap_or(i64::MAX);
    let blobs: Vec<Vec<u8>> = {
        let mut stmt = conn.prepare(
            "SELECT blob FROM capability_grants
              WHERE owner_actor_id = ?1 AND holder_pubkey = ?2 AND epoch_end > ?3",
        )?;
        stmt.query_map(params![owner, holder, now_i64], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    if !blobs.iter().any(|b| grant_blob_admits(b, class, now)) {
        return Ok(Err(Unauthorized));
    }

    // The class's own policy: the kinds never signed for a principal.
    if req.method == Nip46Method::SignEvent {
        match req.sign_event_kind() {
            Ok(kind) if oracle_sign_event_kind_admitted(kind) => {}
            _ => return Ok(Err(Unauthorized)),
        }
    }

    // The rate ceiling; a refused operation does not consume the window.
    let window = RateWindow {
        window_start,
        count: window_count,
    };
    let Ok(next) = window.admit(now) else {
        return Ok(Err(RateLimited));
    };
    conn.execute(
        "UPDATE nostr_oracle_clients SET window_start = ?2, window_count = ?3 WHERE id = ?1",
        params![client_id, next.window_start, next.count],
    )?;
    Ok(Ok(OracleAdmission {
        client_id,
        class: Some(class),
    }))
}

/// The per-operation detail recorded beside the class: the event kind for
/// `sign_event`, the method for the NIP-44 pair.
fn op_detail(req: &Nip46Request) -> Result<String> {
    Ok(match &req.method {
        Nip46Method::SignEvent => req.sign_event_kind()?.to_string(),
        Nip46Method::Nip44Encrypt => "nip44_encrypt".to_string(),
        Nip46Method::Nip44Decrypt => "nip44_decrypt".to_string(),
        other => bail!("no oracle operation record for {other:?}"),
    })
}

/// Record a served request: the client's counters, and — for a class
/// operation — one `nostr_oracle_ops` row, trimming the client's record to
/// the newest [`OPS_KEPT_PER_CLIENT`] in the same write.
pub fn record(
    conn: &Connection,
    actor_hex: &str,
    admission: OracleAdmission,
    req: &Nip46Request,
    now: u64,
) -> Result<()> {
    conn.execute(
        "UPDATE nostr_oracle_clients SET use_count = use_count + 1, last_used_at = ?2
          WHERE id = ?1",
        params![admission.client_id, now],
    )?;
    let Some(class) = admission.class else {
        return Ok(());
    };
    conn.execute(
        "INSERT INTO nostr_oracle_ops (actor_id, client_id, at, class, detail)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            actor_hex,
            admission.client_id,
            now,
            class.name(),
            op_detail(req)?
        ],
    )?;
    conn.execute(
        "DELETE FROM nostr_oracle_ops
          WHERE client_id = ?1 AND id NOT IN
            (SELECT id FROM nostr_oracle_ops WHERE client_id = ?1 ORDER BY id DESC LIMIT ?2)",
        params![admission.client_id, OPS_KEPT_PER_CLIENT],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, params: &[&str]) -> Nip46Request {
        Nip46Request {
            id: "1".into(),
            method: Nip46Method::parse(method),
            params: params.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn client_pubkey_shape() {
        assert!(is_client_pubkey(&"ab".repeat(32)));
        assert!(!is_client_pubkey(&"AB".repeat(32)));
        assert!(!is_client_pubkey(&"ab".repeat(31)));
        assert!(!is_client_pubkey(&"zz".repeat(32)));
    }

    /// The deny list's runtime half at this custodian: no sovereign name is
    /// an operation class, so no grant tuple naming one admits anything —
    /// the compile-time half is `fauna_core::identity_op::DENY_LIST_PINNED`.
    #[test]
    fn a_grant_naming_a_sovereign_operation_admits_no_class() {
        use fauna_core::identity_op::SovereignOp;
        for op in SovereignOp::ALL {
            assert!(IdentityOpClass::parse(op.name()).is_err());
            for class in IdentityOpClass::ALL {
                assert!(!grant_admits(
                    [GrantTupleRef {
                        class: fauna_core::identity_op::CLASS,
                        kind: Some(op.name()),
                    }],
                    *class,
                ));
            }
        }
    }

    #[test]
    fn the_operation_record_names_kind_or_method() {
        assert_eq!(
            op_detail(&req("sign_event", &[r#"{"kind":1,"content":""}"#])).unwrap(),
            "1"
        );
        assert_eq!(
            op_detail(&req("nip44_decrypt", &["x", "y"])).unwrap(),
            "nip44_decrypt"
        );
        assert!(op_detail(&req("ping", &[])).is_err());
    }
}
