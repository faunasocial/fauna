//! NIP-46 bunker: the nest as the user's signer.
//!
//! `docs/goal/ui/nostr.md` § The nest as the user's NIP-46 signer (ratified
//! 2026-07-19). This module owns policy + method execution: the nest-enforced
//! connection roster (deliberately NOT `capability_grants` — the nest is the
//! enforcement point), the per-account signer keypair, the invite/secret
//! lifecycle, and per-request authorization. Protocol crypto (request
//! decrypt, response build) lives in `fauna_bridge_nostr::nip46`; the
//! unauthenticated kind-24133 relay carve-out is `relay_endpoint`'s.

use anyhow::{Context, Result};
use fauna_bridge_nostr::nip46::{Nip46Method, Nip46Request, build_response_json};
use fauna_bridge_nostr::signing::Keypair;
use fauna_bridge_nostr::types::{Event, Tag, UnsignedEvent};
use rusqlite::{Connection, OptionalExtension, params};

use super::{db, key_crypto};

// Hard constants (`nostr.md` § Expiry — no config surface; a human never
// chooses these).
/// How long a minted invite secret stays redeemable.
pub const INVITE_TTL_SECS: u64 = 10 * 60;
/// Sliding idle expiry for an active connection — an active app never
/// breaks; an abandoned one lapses.
pub const IDLE_EXPIRY_SECS: u64 = 90 * 24 * 60 * 60;
/// Per-account cap on pending + active connections.
pub const MAX_APPS_PER_ACCOUNT: i64 = 16;
/// Per-signer request budget per minute (the relay carve-out's limiter,
/// keyed like the gift-wrap one).
pub const BUNKER_REQS_PER_MINUTE: u32 = 60;

/// A freshly minted invite: what the client renders as
/// `bunker://<signer_pubkey>?relay=wss://<domain>/nostr&secret=<secret>`.
/// The secret is one-time-revealed — only its BLAKE3 hash rests in the DB.
pub struct BunkerInvite {
    pub connection_id: i64,
    pub signer_pubkey: String,
    pub secret: String,
}

/// A roster row served to the owner's client (list / revoke / set_label).
#[derive(Debug, Clone)]
pub struct BunkerApp {
    pub id: i64,
    pub app_pubkey: Option<String>,
    pub label: String,
    pub status: String,
    pub created_at: u64,
    pub last_used_at: Option<u64>,
    pub use_count: u64,
    pub expires_at: u64,
}

fn secret_hash(secret: &str) -> [u8; 32] {
    *blake3::hash(secret.as_bytes()).as_bytes()
}

/// The account's signer keypair, minted on first use. Distinct from the
/// user's keypair by design (`nostr.md` § Signer identity).
fn get_or_mint_signer(
    conn: &Connection,
    nest_key: &[u8; 32],
    actor_id: &str,
    now: u64,
) -> Result<Keypair> {
    let existing: Option<Vec<u8>> = conn
        .query_row(
            "SELECT encrypted_privkey FROM nostr_bunker_signers WHERE actor_id = ?1",
            [actor_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(enc) = existing {
        let secret = key_crypto::decrypt_bunker_signer_privkey(nest_key, &enc)?;
        return Keypair::from_secret_bytes(secret);
    }
    let kp = Keypair::generate();
    let enc = key_crypto::encrypt_bunker_signer_privkey(nest_key, &kp.secret_bytes())?;
    conn.execute(
        "INSERT INTO nostr_bunker_signers (actor_id, signer_pubkey, encrypted_privkey, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![actor_id, kp.public_key_hex(), enc, now],
    )?;
    Ok(kp)
}

/// Mint a pending connection with a one-time secret. Requires a linked
/// account in a custodial mode — a `remote`/`nip07` account has no deposited
/// key to sign with, so the bunker is never offered for it.
///
/// The account's first invite mints its signer, sealed under the deployment
/// seed `nest_keypair` holds, read here on `conn` — under the database guard
/// the caller holds for the whole call — never a serving generation's copy
/// (`crate::nest_kek`'s module docs). A generation not yet torn down after a
/// deployment-seed rotation still holds the retired seed, and a signer sealed
/// under it would never open again.
pub fn create_invite(conn: &Connection, actor_id: &str, now: u64) -> Result<BunkerInvite> {
    let account = db::get_account(conn, actor_id)?.context("no linked Nostr account")?;
    if account.encrypted_privkey.is_none() {
        anyhow::bail!("bunker requires a deposited key (custodial signing mode)");
    }

    // Lapsed invites are dead rows (secret unredeemable) — purge before the
    // cap check so abandoned invites never wedge the account at the cap.
    conn.execute(
        "DELETE FROM nostr_bunker_apps
         WHERE actor_id = ?1 AND status = 'pending' AND expires_at <= ?2",
        params![actor_id, now],
    )?;
    let live: i64 = conn.query_row(
        "SELECT COUNT(*) FROM nostr_bunker_apps
         WHERE actor_id = ?1 AND status IN ('pending', 'active')",
        [actor_id],
        |r| r.get(0),
    )?;
    if live >= MAX_APPS_PER_ACCOUNT {
        anyhow::bail!("connected-app limit reached ({MAX_APPS_PER_ACCOUNT})");
    }

    let deployment_seed = crate::nest_kek::require_deployment_seed(conn)?;
    let signer = get_or_mint_signer(conn, &deployment_seed, actor_id, now)?;

    let secret = fauna_core::identity::random_hex(16);
    conn.execute(
        "INSERT INTO nostr_bunker_apps
         (actor_id, app_pubkey, label, secret_hash, status, created_at, expires_at)
         VALUES (?1, NULL, '', ?2, 'pending', ?3, ?4)",
        params![
            actor_id,
            secret_hash(&secret).as_slice(),
            now,
            now + INVITE_TTL_SECS
        ],
    )?;
    Ok(BunkerInvite {
        connection_id: conn.last_insert_rowid(),
        signer_pubkey: signer.public_key_hex(),
        secret,
    })
}

/// Bind a third-party principal's NIP-46 client key to the account's signer
/// (TP11, `fauna.nostr.bunker.bind`; the policy is [`super::oracle`]'s).
/// Same custodial precondition as [`create_invite`], and the same signer:
/// minted here on first use, sealed under the deployment seed read on `conn`.
/// Returns the signer pubkey, or the oracle's refusal.
pub fn bind_client(
    conn: &Connection,
    actor_id: &str,
    principal_id: &[u8],
    client_pubkey: &str,
    now: u64,
) -> Result<std::result::Result<String, super::oracle::BindRefusal>> {
    let account = db::get_account(conn, actor_id)?.context("no linked Nostr account")?;
    if account.encrypted_privkey.is_none() {
        anyhow::bail!("bunker requires a deposited key (custodial signing mode)");
    }
    let deployment_seed = crate::nest_kek::require_deployment_seed(conn)?;
    let signer = get_or_mint_signer(conn, &deployment_seed, actor_id, now)?;
    Ok(
        super::oracle::bind(conn, actor_id, principal_id, client_pubkey, now)?
            .map(|()| signer.public_key_hex()),
    )
}

/// The owner's roster view: pending + active rows (revoked tombstones are
/// not listed — the mail-credentials row-disappears precedent).
pub fn list_apps(conn: &Connection, actor_id: &str) -> Result<Vec<BunkerApp>> {
    let mut stmt = conn.prepare(
        "SELECT id, app_pubkey, label, status, created_at, last_used_at, use_count, expires_at
         FROM nostr_bunker_apps
         WHERE actor_id = ?1 AND status != 'revoked'
         ORDER BY created_at DESC, id DESC",
    )?;
    let rows = stmt
        .query_map([actor_id], |r| {
            Ok(BunkerApp {
                id: r.get(0)?,
                app_pubkey: r.get(1)?,
                label: r.get(2)?,
                status: r.get(3)?,
                created_at: r.get(4)?,
                last_used_at: r.get(5)?,
                use_count: r.get(6)?,
                expires_at: r.get(7)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Revoke a connection (caller-scoped). Immediate — authorization is
/// evaluated per request, so no cached authority survives. Returns false if
/// no live row matched.
pub fn revoke_app(conn: &Connection, actor_id: &str, connection_id: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE nostr_bunker_apps SET status = 'revoked', secret_hash = NULL
         WHERE id = ?1 AND actor_id = ?2 AND status != 'revoked'",
        params![connection_id, actor_id],
    )?;
    Ok(n > 0)
}

/// Set a connection's user-editable label (the bunker flow transports no app
/// name). Returns false if no live row matched.
pub fn set_label(
    conn: &Connection,
    actor_id: &str,
    connection_id: i64,
    label: &str,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE nostr_bunker_apps SET label = ?3
         WHERE id = ?1 AND actor_id = ?2 AND status != 'revoked'",
        params![connection_id, actor_id, label],
    )?;
    Ok(n > 0)
}

/// Signer-registry membership: the F4-position-2 check the relay carve-out
/// runs after signature verification. None → not a bunker signer.
pub fn signer_actor(conn: &Connection, signer_pubkey: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT actor_id FROM nostr_bunker_signers WHERE signer_pubkey = ?1",
            [signer_pubkey],
            |r| r.get(0),
        )
        .optional()?)
}

/// Decrypt the signer keypair for a registered signer pubkey.
pub fn signer_keypair(
    conn: &Connection,
    nest_key: &[u8; 32],
    signer_pubkey: &str,
) -> Result<Option<Keypair>> {
    let enc: Option<Vec<u8>> = conn
        .query_row(
            "SELECT encrypted_privkey FROM nostr_bunker_signers WHERE signer_pubkey = ?1",
            [signer_pubkey],
            |r| r.get(0),
        )
        .optional()?;
    match enc {
        None => Ok(None),
        Some(enc) => {
            let secret = key_crypto::decrypt_bunker_signer_privkey(nest_key, &enc)?;
            Ok(Some(Keypair::from_secret_bytes(secret)?))
        }
    }
}

/// The active-connection check, evaluated per request (revocation and expiry
/// are immediate). Returns the row id to record use against.
fn authorized_app(
    conn: &Connection,
    actor_id: &str,
    app_pubkey: &str,
    now: u64,
) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM nostr_bunker_apps
             WHERE actor_id = ?1 AND app_pubkey = ?2 AND status = 'active' AND expires_at > ?3",
            params![actor_id, app_pubkey, now],
            |r| r.get(0),
        )
        .optional()?)
}

/// Record a served request: audit counters + the sliding idle expiry.
fn record_use(conn: &Connection, connection_id: i64, now: u64) -> Result<()> {
    conn.execute(
        "UPDATE nostr_bunker_apps
         SET last_used_at = ?2, use_count = use_count + 1, expires_at = ?3
         WHERE id = ?1",
        params![connection_id, now, now + IDLE_EXPIRY_SECS],
    )?;
    Ok(())
}

/// The user's signing keypair from the deposited nsec — the established
/// signing position (`mod.rs::propagate_post_delete`,
/// `store::materialize_all_exposed`). Every bunker act signs/encrypts under
/// this key; the signer keypair only transports.
fn user_keypair(conn: &Connection, nest_key: &[u8; 32], actor_id: &str) -> Result<Keypair> {
    let account = db::get_account(conn, actor_id)?.context("account unlinked")?;
    let enc = account
        .encrypted_privkey
        .context("no deposited key for account")?;
    let secret = key_crypto::decrypt_nostr_privkey(nest_key, &enc)?;
    Keypair::from_secret_bytes(secret)
}

/// Handle one decrypted NIP-46 request addressed to `signer_pubkey`, authored
/// by `app_pubkey` (both already signature-verified by the relay carve-out).
/// Always produces a response JSON — method/authorization failures become
/// `{"id", "error"}` (never a silent drop); `Err` is reserved for
/// infrastructure failure (DB unavailable).
pub fn handle_request(
    conn: &Connection,
    nest_key: &[u8; 32],
    signer_pubkey: &str,
    app_pubkey: &str,
    req: &Nip46Request,
    now: u64,
) -> Result<String> {
    let Some(actor_id) = signer_actor(conn, signer_pubkey)? else {
        return Ok(build_response_json(&req.id, Err("unauthorized")));
    };

    if req.method == Nip46Method::Connect {
        return handle_connect(conn, &actor_id, app_pubkey, req, now);
    }

    let Some(connection_id) = authorized_app(conn, &actor_id, app_pubkey, now)? else {
        // No invite-roster app: the author may be a third-party principal's
        // bound client — the oracle re-resolves its grant per request.
        return match super::oracle::authorize(conn, &actor_id, app_pubkey, req, now)? {
            Err(refusal) => Ok(build_response_json(&req.id, Err(refusal.wire()))),
            Ok(admission) => match execute_method(conn, nest_key, &actor_id, req, now) {
                Ok(result) => {
                    super::oracle::record(conn, &actor_id, admission, req, now)?;
                    Ok(build_response_json(&req.id, Ok(&result)))
                }
                Err(e) => Ok(build_response_json(&req.id, Err(&e.to_string()))),
            },
        };
    };

    match execute_method(conn, nest_key, &actor_id, req, now) {
        Ok(result) => {
            record_use(conn, connection_id, now)?;
            Ok(build_response_json(&req.id, Ok(&result)))
        }
        Err(e) => Ok(build_response_json(&req.id, Err(&e.to_string()))),
    }
}

/// The shared NIP-46 execute-and-respond core: decrypt an app's kind-24133
/// `request` addressed to the locally-registered `signer_pubkey`, run it, and
/// build the signer-authored, app-encrypted response event to transport.
///
/// Both transports call this identically — only how the response is carried
/// differs: the public relay carve-out ([`relay_endpoint::handle_bunker_request`](crate::nostr::relay_endpoint::handle_bunker_request))
/// broadcasts it over `relay_tx`; the head's proxy subscription
/// ([`sync_worker`](crate::nostr::sync_worker)) publishes it back to the paired
/// public box's relay (R10 (account-data-plane.md § The ratified decisions)). It assumes the caller already **verified the
/// request signature** (the relay carve-out's F4 ordering; the head trusts the
/// peer relay's own accept gate) and already **confirmed signer membership** —
/// this is the execute core, not the transport gate, so it never re-broadcasts
/// the request nor rate-limits.
///
/// `Err` is a genuine processing failure (malformed author pubkey, missing
/// signer key, undecryptable/unparseable request) the caller logs; a NIP-46
/// method/authorization failure is **not** an `Err` — [`handle_request`] folds
/// it into a `{"id","error"}` response event, so a well-formed-but-unauthorized
/// request still round-trips a signed error back to the app (never a silent
/// drop).
pub fn execute_bunker_request(
    conn: &Connection,
    nest_key: &[u8; 32],
    signer_pubkey: &str,
    request: &Event,
    now: u64,
) -> Result<Event> {
    let mut app_pubkey = [0u8; 32];
    match hex::decode(&request.pubkey) {
        Ok(bytes) if bytes.len() == 32 => app_pubkey.copy_from_slice(&bytes),
        _ => anyhow::bail!("malformed author pubkey"),
    }

    let signer_kp = signer_keypair(conn, nest_key, signer_pubkey)?
        .context("signer keypair unavailable for a registered signer")?;

    let (req, scheme) =
        fauna_bridge_nostr::nip46::decrypt_request(&signer_kp, &app_pubkey, &request.content)
            .context("decrypt/parse bunker request")?;

    let response_json = handle_request(conn, nest_key, signer_pubkey, &request.pubkey, &req, now)?;

    fauna_bridge_nostr::nip46::build_response_event(
        &signer_kp,
        &app_pubkey,
        scheme,
        &response_json,
        now,
    )
}

/// `connect`: consume the one-time secret and pin the app pubkey. A repeat
/// `connect` from an already-active app pubkey is an idempotent "ack"
/// (standard client reconnect); an unknown/expired/reused secret is
/// "unauthorized".
fn handle_connect(
    conn: &Connection,
    actor_id: &str,
    app_pubkey: &str,
    req: &Nip46Request,
    now: u64,
) -> Result<String> {
    if let Some(connection_id) = authorized_app(conn, actor_id, app_pubkey, now)? {
        record_use(conn, connection_id, now)?;
        return Ok(build_response_json(&req.id, Ok("ack")));
    }

    // A principal's bound client needs no secret: its key is already pinned
    // by `fauna.nostr.bunker.bind`, and the oracle admits `connect` on the
    // client and principal rows alone (the grant is checked per operation).
    if let Ok(admission) = super::oracle::authorize(conn, actor_id, app_pubkey, req, now)? {
        super::oracle::record(conn, actor_id, admission, req, now)?;
        return Ok(build_response_json(&req.id, Ok("ack")));
    }

    let Some(secret) = req.connect_secret() else {
        return Ok(build_response_json(&req.id, Err("unauthorized")));
    };
    let presented = blake3::hash(secret.as_bytes());

    let mut stmt = conn.prepare(
        "SELECT id, secret_hash FROM nostr_bunker_apps
         WHERE actor_id = ?1 AND status = 'pending' AND expires_at > ?2",
    )?;
    let pending: Vec<(i64, Vec<u8>)> = stmt
        .query_map(params![actor_id, now], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    // blake3::Hash equality is constant-time; compare via Hash, not bytes.
    let matched = pending.iter().find(|(_, stored)| {
        <[u8; 32]>::try_from(stored.as_slice())
            .map(|arr| blake3::Hash::from_bytes(arr) == presented)
            .unwrap_or(false)
    });
    let Some((connection_id, _)) = matched else {
        return Ok(build_response_json(&req.id, Err("unauthorized")));
    };

    // Single-use consume: activate, pin the app pubkey, clear the hash.
    conn.execute(
        "UPDATE nostr_bunker_apps
         SET status = 'active', app_pubkey = ?2, secret_hash = NULL,
             last_used_at = ?3, use_count = use_count + 1, expires_at = ?4
         WHERE id = ?1",
        params![connection_id, app_pubkey, now, now + IDLE_EXPIRY_SECS],
    )?;
    Ok(build_response_json(&req.id, Ok("ack")))
}

fn execute_method(
    conn: &Connection,
    nest_key: &[u8; 32],
    actor_id: &str,
    req: &Nip46Request,
    now: u64,
) -> Result<String> {
    match &req.method {
        // Handled before dispatch; defensive.
        Nip46Method::Connect => anyhow::bail!("connect handled separately"),
        Nip46Method::Ping => Ok("pong".to_string()),
        Nip46Method::GetPublicKey => {
            // The USER pubkey — signer-pubkey ≠ user-pubkey by design.
            let account = db::get_account(conn, actor_id)?.context("account unlinked")?;
            Ok(account.nostr_pubkey)
        }
        Nip46Method::SignEvent => {
            let kp = user_keypair(conn, nest_key, actor_id)?;
            let payload = req.sign_event_payload()?;
            let v: serde_json::Value =
                serde_json::from_str(payload).context("sign_event payload is not JSON")?;
            let kind = v
                .get("kind")
                .and_then(|k| k.as_u64())
                .context("sign_event payload missing kind")?;
            let content = v
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or_default()
                .to_string();
            let tags: Vec<Tag> = match v.get("tags") {
                None => Vec::new(),
                Some(t) => {
                    serde_json::from_value(t.clone()).context("sign_event payload bad tags")?
                }
            };
            let created_at = v.get("created_at").and_then(|c| c.as_u64()).unwrap_or(now);
            let unsigned = UnsignedEvent {
                pubkey: kp.public_key_bytes(),
                created_at,
                kind,
                tags,
                content,
            };
            let event = kp.sign_event(unsigned);
            Ok(serde_json::to_string(&event)?)
        }
        Nip46Method::Nip44Encrypt => {
            let kp = user_keypair(conn, nest_key, actor_id)?;
            let third_party = req.crypt_third_party_pubkey()?;
            fauna_bridge_nostr::nip44::nip44_encrypt(
                &kp.secret_bytes(),
                &third_party,
                req.crypt_payload()?,
            )
        }
        Nip46Method::Nip44Decrypt => {
            let kp = user_keypair(conn, nest_key, actor_id)?;
            let third_party = req.crypt_third_party_pubkey()?;
            fauna_bridge_nostr::nip44::nip44_decrypt(
                &kp.secret_bytes(),
                &third_party,
                req.crypt_payload()?,
            )
        }
        Nip46Method::Nip04Encrypt => {
            let kp = user_keypair(conn, nest_key, actor_id)?;
            let third_party = req.crypt_third_party_pubkey()?;
            fauna_bridge_nostr::nip04::encrypt(&kp, &third_party, req.crypt_payload()?)
        }
        Nip46Method::Nip04Decrypt => {
            let kp = user_keypair(conn, nest_key, actor_id)?;
            let third_party = req.crypt_third_party_pubkey()?;
            fauna_bridge_nostr::nip04::decrypt(&kp, &third_party, req.crypt_payload()?)
        }
        Nip46Method::Unknown(m) => anyhow::bail!("unsupported method: {m}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEST_KEY: [u8; 32] = [42u8; 32];

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run_migrations(&conn).unwrap();
        crate::nostr::apply_schema(&conn).unwrap();
        // The deployment keypair an invite's signer is sealed under.
        let public = ed25519_dalek::SigningKey::from_bytes(&NEST_KEY)
            .verifying_key()
            .to_bytes();
        conn.execute(
            "INSERT OR REPLACE INTO nest_keypair (id, secret_key, public_key, created_at)
             VALUES (1, ?1, ?2, 0)",
            params![NEST_KEY.as_slice(), public.as_slice()],
        )
        .unwrap();
        conn
    }

    /// A linked custodial account with a real deposited key; returns the
    /// user's keypair.
    fn link_custodial(conn: &Connection, actor_id: &str) -> Keypair {
        let user_kp = Keypair::generate();
        let enc = key_crypto::encrypt_nostr_privkey(&NEST_KEY, &user_kp.secret_bytes()).unwrap();
        db::link_account(
            conn,
            actor_id,
            &user_kp.public_key_hex(),
            "generated",
            Some(&enc),
            None,
            None,
        )
        .unwrap();
        user_kp
    }

    fn request(id: &str, method: &str, params: Vec<String>) -> Nip46Request {
        Nip46Request {
            id: id.into(),
            method: Nip46Method::parse(method),
            params,
        }
    }

    /// Full connect flow; returns (signer_pubkey, app keypair pubkey hex).
    fn connect_app(conn: &Connection, actor_id: &str, now: u64) -> (String, String) {
        let invite = create_invite(conn, actor_id, now).unwrap();
        let app_pubkey = Keypair::generate().public_key_hex();
        let req = request(
            "c1",
            "connect",
            vec![invite.signer_pubkey.clone(), invite.secret],
        );
        let resp = handle_request(
            conn,
            &NEST_KEY,
            &invite.signer_pubkey,
            &app_pubkey,
            &req,
            now,
        )
        .unwrap();
        assert!(
            resp.contains("\"result\":\"ack\""),
            "connect failed: {resp}"
        );
        (invite.signer_pubkey, app_pubkey)
    }

    #[test]
    fn invite_requires_custodial_account() {
        let conn = test_conn();
        // No account at all.
        assert!(create_invite(&conn, "nobody", 100).is_err());
        // Linked, but no deposited key (remote signing mode).
        db::link_account(&conn, "remoteguy", "pk_remote", "remote", None, None, None).unwrap();
        assert!(create_invite(&conn, "remoteguy", 100).is_err());
    }

    #[test]
    fn signer_is_stable_across_invites_and_registered() {
        let conn = test_conn();
        link_custodial(&conn, "a1");
        let i1 = create_invite(&conn, "a1", 100).unwrap();
        let i2 = create_invite(&conn, "a1", 101).unwrap();
        assert_eq!(i1.signer_pubkey, i2.signer_pubkey);
        assert_ne!(i1.secret, i2.secret);
        assert_eq!(
            signer_actor(&conn, &i1.signer_pubkey).unwrap().as_deref(),
            Some("a1")
        );
        assert!(signer_actor(&conn, "ff00").unwrap().is_none());
        // The signer keypair decrypts and matches the registered pubkey.
        let kp = signer_keypair(&conn, &NEST_KEY, &i1.signer_pubkey)
            .unwrap()
            .unwrap();
        assert_eq!(kp.public_key_hex(), i1.signer_pubkey);
        // And it is NOT the user's pubkey (signer identity is dedicated).
        let account = db::get_account(&conn, "a1").unwrap().unwrap();
        assert_ne!(kp.public_key_hex(), account.nostr_pubkey);
    }

    #[test]
    fn per_account_cap_enforced_and_expired_invites_purged() {
        let conn = test_conn();
        link_custodial(&conn, "a1");
        for _ in 0..MAX_APPS_PER_ACCOUNT {
            create_invite(&conn, "a1", 100).unwrap();
        }
        assert!(create_invite(&conn, "a1", 100).is_err());
        // Once the pending invites lapse, the cap frees up again.
        let later = 100 + INVITE_TTL_SECS;
        assert!(create_invite(&conn, "a1", later).is_ok());
    }

    #[test]
    fn connect_consumes_secret_single_use() {
        let conn = test_conn();
        link_custodial(&conn, "a1");
        let invite = create_invite(&conn, "a1", 100).unwrap();
        let app1 = Keypair::generate().public_key_hex();
        let req = request(
            "c1",
            "connect",
            vec![invite.signer_pubkey.clone(), invite.secret.clone()],
        );
        let resp =
            handle_request(&conn, &NEST_KEY, &invite.signer_pubkey, &app1, &req, 100).unwrap();
        assert!(resp.contains("\"result\":\"ack\""));

        // Same secret from a different app: refused (single-use).
        let app2 = Keypair::generate().public_key_hex();
        let resp2 =
            handle_request(&conn, &NEST_KEY, &invite.signer_pubkey, &app2, &req, 101).unwrap();
        assert!(resp2.contains("\"error\":\"unauthorized\""), "{resp2}");

        // Reconnect from the SAME app: idempotent ack.
        let resp3 =
            handle_request(&conn, &NEST_KEY, &invite.signer_pubkey, &app1, &req, 102).unwrap();
        assert!(resp3.contains("\"result\":\"ack\""), "{resp3}");
    }

    #[test]
    fn connect_rejects_expired_or_wrong_secret() {
        let conn = test_conn();
        link_custodial(&conn, "a1");
        let invite = create_invite(&conn, "a1", 100).unwrap();
        let app = Keypair::generate().public_key_hex();

        // Wrong secret.
        let bad = request(
            "c1",
            "connect",
            vec![invite.signer_pubkey.clone(), "deadbeef".into()],
        );
        let resp =
            handle_request(&conn, &NEST_KEY, &invite.signer_pubkey, &app, &bad, 100).unwrap();
        assert!(resp.contains("\"error\":\"unauthorized\""));

        // Right secret, after the TTL.
        let late = request(
            "c2",
            "connect",
            vec![invite.signer_pubkey.clone(), invite.secret.clone()],
        );
        let expired_now = 100 + INVITE_TTL_SECS;
        let resp2 = handle_request(
            &conn,
            &NEST_KEY,
            &invite.signer_pubkey,
            &app,
            &late,
            expired_now,
        )
        .unwrap();
        assert!(resp2.contains("\"error\":\"unauthorized\""));
    }

    #[test]
    fn unregistered_signer_is_unauthorized() {
        let conn = test_conn();
        link_custodial(&conn, "a1");
        let req = request("x", "ping", vec![]);
        let resp = handle_request(
            &conn,
            &NEST_KEY,
            &"ee".repeat(32),
            &"aa".repeat(32),
            &req,
            100,
        )
        .unwrap();
        assert!(resp.contains("\"error\":\"unauthorized\""));
    }

    #[test]
    fn methods_execute_under_the_user_key() {
        let conn = test_conn();
        let user_kp = link_custodial(&conn, "a1");
        let (signer_pk, app_pk) = connect_app(&conn, "a1", 100);

        // ping
        let resp = handle_request(
            &conn,
            &NEST_KEY,
            &signer_pk,
            &app_pk,
            &request("p1", "ping", vec![]),
            101,
        )
        .unwrap();
        assert_eq!(resp, "{\"id\":\"p1\",\"result\":\"pong\"}");

        // get_public_key returns the USER pubkey, not the signer's.
        let resp = handle_request(
            &conn,
            &NEST_KEY,
            &signer_pk,
            &app_pk,
            &request("g1", "get_public_key", vec![]),
            102,
        )
        .unwrap();
        assert!(resp.contains(&user_kp.public_key_hex()), "{resp}");
        assert!(!resp.contains(&signer_pk), "{resp}");

        // sign_event signs under the deposited user key.
        let unsigned_json =
            "{\"kind\":1,\"content\":\"hello from an app\",\"tags\":[],\"created_at\":103}";
        let resp = handle_request(
            &conn,
            &NEST_KEY,
            &signer_pk,
            &app_pk,
            &request("s1", "sign_event", vec![unsigned_json.into()]),
            103,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let signed: fauna_bridge_nostr::types::Event =
            serde_json::from_str(v["result"].as_str().unwrap()).unwrap();
        assert_eq!(signed.kind, 1);
        assert_eq!(signed.pubkey, user_kp.public_key_hex());
        assert!(fauna_bridge_nostr::signing::verify_event(&signed));

        // nip44 encrypt→decrypt round-trip through the bunker.
        let third_party = Keypair::generate();
        let resp = handle_request(
            &conn,
            &NEST_KEY,
            &signer_pk,
            &app_pk,
            &request(
                "e1",
                "nip44_encrypt",
                vec![third_party.public_key_hex(), "sealed words".into()],
            ),
            104,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let ciphertext = v["result"].as_str().unwrap().to_string();
        // The third party can open it as coming from the USER.
        let opened = fauna_bridge_nostr::nip44::nip44_decrypt(
            &third_party.secret_bytes(),
            &user_kp.public_key_bytes(),
            &ciphertext,
        )
        .unwrap();
        assert_eq!(opened, "sealed words");
        // And the bunker can decrypt the reverse direction.
        let inbound = fauna_bridge_nostr::nip44::nip44_encrypt(
            &third_party.secret_bytes(),
            &user_kp.public_key_bytes(),
            "reply words",
        )
        .unwrap();
        let resp = handle_request(
            &conn,
            &NEST_KEY,
            &signer_pk,
            &app_pk,
            &request(
                "d1",
                "nip44_decrypt",
                vec![third_party.public_key_hex(), inbound],
            ),
            105,
        )
        .unwrap();
        assert!(resp.contains("reply words"));

        // Unknown method answers an error (never a silent drop).
        let resp = handle_request(
            &conn,
            &NEST_KEY,
            &signer_pk,
            &app_pk,
            &request("u1", "frobnicate", vec![]),
            106,
        )
        .unwrap();
        assert!(resp.contains("\"error\""), "{resp}");
    }

    #[test]
    fn revoke_is_immediate_and_idle_expiry_slides() {
        let conn = test_conn();
        link_custodial(&conn, "a1");
        let (signer_pk, app_pk) = connect_app(&conn, "a1", 100);

        // Sliding expiry: a request at t bumps expires_at to t + IDLE.
        handle_request(
            &conn,
            &NEST_KEY,
            &signer_pk,
            &app_pk,
            &request("p1", "ping", vec![]),
            200,
        )
        .unwrap();
        let apps = list_apps(&conn, "a1").unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].expires_at, 200 + IDLE_EXPIRY_SECS);
        assert_eq!(apps[0].last_used_at, Some(200));
        assert!(apps[0].use_count >= 2); // connect + ping

        // Past the idle window with no use: refused.
        let idle_now = 200 + IDLE_EXPIRY_SECS;
        let resp = handle_request(
            &conn,
            &NEST_KEY,
            &signer_pk,
            &app_pk,
            &request("p2", "ping", vec![]),
            idle_now,
        )
        .unwrap();
        assert!(resp.contains("\"error\":\"unauthorized\""));

        // Fresh connection, then revoke: next request refused, row unlisted.
        let (signer_pk2, app_pk2) = connect_app(&conn, "a1", 300);
        let id = list_apps(&conn, "a1")
            .unwrap()
            .into_iter()
            .find(|a| a.status == "active" && a.app_pubkey.as_deref() == Some(app_pk2.as_str()))
            .unwrap()
            .id;
        assert!(revoke_app(&conn, "a1", id).unwrap());
        let resp = handle_request(
            &conn,
            &NEST_KEY,
            &signer_pk2,
            &app_pk2,
            &request("p3", "ping", vec![]),
            301,
        )
        .unwrap();
        assert!(resp.contains("\"error\":\"unauthorized\""));
        assert!(
            !list_apps(&conn, "a1").unwrap().iter().any(|a| a.id == id),
            "revoked row must not be listed"
        );
        // Revoking again reports no live row.
        assert!(!revoke_app(&conn, "a1", id).unwrap());
    }

    #[test]
    fn labels_are_caller_scoped() {
        let conn = test_conn();
        link_custodial(&conn, "a1");
        link_custodial(&conn, "a2");
        let (_, _) = connect_app(&conn, "a1", 100);
        let id = list_apps(&conn, "a1").unwrap()[0].id;
        // The owner can label; another actor cannot touch it.
        assert!(set_label(&conn, "a1", id, "Damus on phone").unwrap());
        assert!(!set_label(&conn, "a2", id, "hijack").unwrap());
        assert!(!revoke_app(&conn, "a2", id).unwrap());
        assert_eq!(list_apps(&conn, "a1").unwrap()[0].label, "Damus on phone");
    }

    #[test]
    fn unlink_cascade_kills_live_connections() {
        let conn = test_conn();
        link_custodial(&conn, "a1");
        let (signer_pk, app_pk) = connect_app(&conn, "a1", 100);
        db::unlink_account(&conn, "a1").unwrap();
        let resp = handle_request(
            &conn,
            &NEST_KEY,
            &signer_pk,
            &app_pk,
            &request("p1", "ping", vec![]),
            101,
        )
        .unwrap();
        // Signer row dropped by the cascade → not a registered signer.
        assert!(resp.contains("\"error\":\"unauthorized\""));
    }
}
