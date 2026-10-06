//! The nest-held **DKIM signing keys** — one per (mail domain, selector) — and
//! the one site that signs with them.
//!
//! `docs/goal/behavior/mail-bridge-lifecycle.md` § DKIM provisioning
//! (automatic) → *Custody moves to the nest* owns the custody decision, when a
//! key mints and the sign site;
//! `docs/goal/architecture/key-material-hierarchy.md` § Audience: deployment
//! infrastructure → *The oracle* → *The DKIM class is the outbound spool's own
//! door* owns the key's entry. This module is both made structural: the table
//! is written and opened only here.
//!
//! A key is a member of the nest-internal key-encryption family
//! ([`crate::nest_kek`]): the private half rests in `mail_dkim_keys.key_wrapped`
//! sealed under the deployment seed with its own dated context
//! ([`MAIL_DKIM_CONTEXT`]), registered in `nest_kek::SATELLITES` so a
//! deployment-seed rotation re-wraps it and the published selector stays valid.
//! The public half rests beside it unsealed — it is the DNS record.
//!
//! # When a key mints — never by a read
//!
//! Three doors, each sealing under the seed read from `nest_keypair` on the
//! connection it inserts on (`key-material-hierarchy.md` § Room-read keypair →
//! *When it mints*):
//!
//! - the boot step, for every active mail domain whose active selector has no
//!   key ([`mint_for_keyless_domains`], called with the seed the boot step
//!   read);
//! - the doors that activate a mail domain — adding or restoring one
//!   ([`mint_selector`]);
//! - the scheduled rotation mint ([`mint_selector`]).
//!
//! [`OutboundSigner::load`] and every other read only look a key up. No bridge
//! holds a DKIM key: the nest is the only signer.
//!
//! # Carried keys — a factory reset keeps the published record valid
//!
//! `docs/goal/architecture/nest/common.md` § Factory reset → *The DKIM keys are
//! carried* owns the rule. The reset deletes the database, so before it does
//! [`carry_across_factory_reset`] copies each active domain's active-selector
//! key — still sealed — into [`CARRY_FILE`] in the data dir, and the boot that
//! follows seats them on the fresh database with `carried_at` stamped
//! ([`CacheDb::receive_carried_dkim_keys`]). A carried row is no selector to
//! any reader: it is held for the door that re-registers its domain, which
//! adopts it in place of minting ([`mint`] clears the stamp;
//! [`carried_selector`] tells the add door which selector to register under).
//! One nobody claims expires with the soft-delete window ([`expire_carried`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use fauna_mail::outbound::dkim::{SigningAlg, SigningKey};
use rusqlite::{Connection, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize as _, Zeroizing};

use crate::db::CacheDb;
use crate::nest_kek::{MAIL_DKIM_CONTEXT, deployment_seed, derive, require_deployment_seed};

/// The selector a `mail_domains` row signs under while its `dkim_selector`
/// column is NULL — a domain that has never rotated.
pub const DEFAULT_SELECTOR: &str = "default";

/// Hand-outs that left unsigned although the deployment signs for their From
/// domain: no key for the active selector, a key that would not open, or a
/// sign failure. Since process start; the unit ships regardless.
static UNSIGNED_HANDOUTS: AtomicU64 = AtomicU64::new(0);

/// [`UNSIGNED_HANDOUTS`], read.
pub fn unsigned_handouts() -> u64 {
    UNSIGNED_HANDOUTS.load(Ordering::Relaxed)
}

/// Seal a DKIM private key (the signer's own encoding — PKCS#8 DER for
/// Ed25519, PKCS#8 PEM for RSA) under the deployment seed. Variable length, so
/// the family's `derive` + AEAD directly rather than its 32-byte wrapper.
fn seal_dkim_key(deployment_seed: &[u8; 32], priv_key: &[u8]) -> Result<Vec<u8>> {
    fauna_core::crypto::encrypt_backup_chunk(&derive(MAIL_DKIM_CONTEXT, deployment_seed), priv_key)
}

fn open_dkim_key(deployment_seed: &[u8; 32], wrapped: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    fauna_core::crypto::decrypt_backup_chunk(&derive(MAIL_DKIM_CONTEXT, deployment_seed), wrapped)
        .map(Zeroizing::new)
}

fn holds(conn: &Connection, domain: &str, selector: &str) -> Result<bool> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM mail_dkim_keys
              WHERE domain = ?1 AND selector = ?2 AND carried_at IS NULL",
            rusqlite::params![domain, selector],
            |r| r.get(0),
        )
        .context("probe mail_dkim_keys")?;
    Ok(n > 0)
}

/// Take the key a factory reset carried for `(domain, selector)`, if there is
/// one: clearing the stamp makes it the selector's key. One statement, so a
/// crash leaves it carried or adopted and never both.
fn adopt_carried(conn: &Connection, domain: &str, selector: &str) -> Result<bool> {
    let adopted = conn
        .execute(
            "UPDATE mail_dkim_keys SET carried_at = NULL
              WHERE domain = ?1 AND selector = ?2 AND carried_at IS NOT NULL",
            rusqlite::params![domain, selector],
        )
        .context("adopt a carried DKIM key")?;
    if adopted > 0 {
        tracing::info!(
            target: "mail_dkim",
            domain = %domain,
            selector = %selector,
            "adopted the DKIM signing key a factory reset carried; the published record stays valid"
        );
    }
    Ok(adopted > 0)
}

/// Mint and seat a key for `(domain, selector)` unless one already exists
/// (first-write-wins) or a factory reset carried one, which is adopted
/// instead. `true` when this call minted.
fn mint(
    conn: &Connection,
    deployment_seed: &[u8; 32],
    domain: &str,
    selector: &str,
) -> Result<bool> {
    if holds(conn, domain, selector)? || adopt_carried(conn, domain, selector)? {
        return Ok(false);
    }
    let alg = fauna_provisioning::dkim::DkimAlgorithm::Ed25519;
    let material = fauna_provisioning::dkim::mint_signing_key(alg)
        .map_err(|e| anyhow::anyhow!("mint a DKIM key for {domain}/{selector}: {e}"))?;
    let priv_key = Zeroizing::new(material.priv_key);
    let wrapped = seal_dkim_key(deployment_seed, &priv_key)
        .context("seal the DKIM key under the deployment seed")?;
    let seated = conn
        .execute(
            "INSERT OR IGNORE INTO mail_dkim_keys
                (domain, selector, alg, key_wrapped, public_dns_value, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                domain,
                selector,
                alg.wire_alg(),
                wrapped,
                material.public_dns_value,
                crate::db::now_epoch_millis(),
            ],
        )
        .context("insert the DKIM key")?;
    if seated > 0 {
        tracing::info!(
            target: "mail_dkim",
            domain = %domain,
            selector = %selector,
            "minted a nest-held DKIM signing key"
        );
    }
    Ok(seated > 0)
}

/// A deliberate door mints a key for `(domain, selector)`: the seed is read on
/// `conn`, so the row is sealed under the seed the database holds right then.
pub(crate) fn mint_selector(conn: &Connection, domain: &str, selector: &str) -> Result<bool> {
    let seed = require_deployment_seed(conn)?;
    mint(conn, &seed, domain, selector)
}

/// Seat a key for every active mail domain whose **active** selector has
/// none. Returns the domains minted for.
///
/// The boot step calls it with the seed it read under the same guard, so every
/// box comes up with a key — and a DNS record to publish — for every domain,
/// whether or not a bridge is enrolled.
pub(crate) fn mint_for_keyless_domains(
    conn: &Connection,
    deployment_seed: &[u8; 32],
) -> Result<Vec<String>> {
    let keyless: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT d.domain_name, COALESCE(d.dkim_selector, ?1)
                   FROM mail_domains d
                  WHERE d.removed_at IS NULL
                    AND NOT EXISTS (SELECT 1 FROM mail_dkim_keys k
                                     WHERE k.domain = d.domain_name
                                       AND k.selector = COALESCE(d.dkim_selector, ?1)
                                       AND k.carried_at IS NULL)",
            )
            .context("prepare the keyless-domain read")?;
        stmt.query_map([DEFAULT_SELECTOR], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()
            .context("read the keyless mail domains")?
    };
    let mut minted = Vec::new();
    for (domain, selector) in keyless {
        if mint(conn, deployment_seed, &domain, &selector)? {
            minted.push(domain);
        }
    }
    Ok(minted)
}

/// The data-dir file a factory reset carries the DKIM keys in, from the wipe to
/// the boot that follows it. Sealed keys and public records only.
const CARRY_FILE: &str = "dkim-keys-carried";

fn carry_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CARRY_FILE)
}

/// One carried key: a `mail_dkim_keys` row, `key_wrapped` in hex.
#[derive(Serialize, Deserialize)]
struct CarriedKey {
    domain: String,
    selector: String,
    alg: String,
    key_wrapped: String,
    public_dns_value: String,
    created_at: i64,
}

/// The factory reset's first step, before it deletes the database: copy each
/// active mail domain's active-selector key into [`CARRY_FILE`]. Returns how
/// many it carried.
///
/// The keys stay sealed under the deployment seed, which the reset preserves.
/// A re-run after the database is gone (a crash mid-wipe) carries nothing and
/// leaves the first run's file in place. The caller treats an error as "no key
/// carried": the reset is the recovery floor and never waits on this.
pub(crate) fn carry_across_factory_reset(db_path: &Path, data_dir: &Path) -> Result<usize> {
    if !db_path.exists() {
        return Ok(0);
    }
    let keys: Vec<CarriedKey> = {
        let conn = Connection::open_with_flags(
            db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .context("open the database the reset is about to wipe")?;
        // No `carried_at` filter here: a key an active domain's active
        // selector names is that domain's key, carried before or not.
        let mut stmt = conn
            .prepare(
                "SELECT k.domain, k.selector, k.alg, k.key_wrapped, k.public_dns_value,
                        k.created_at
                   FROM mail_domains d
                   JOIN mail_dkim_keys k
                     ON k.domain = d.domain_name
                    AND k.selector = COALESCE(d.dkim_selector, ?1)
                  WHERE d.removed_at IS NULL",
            )
            .context("prepare the carry read")?;
        stmt.query_map([DEFAULT_SELECTOR], |r| {
            Ok(CarriedKey {
                domain: r.get(0)?,
                selector: r.get(1)?,
                alg: r.get(2)?,
                key_wrapped: hex::encode(r.get::<_, Vec<u8>>(3)?),
                public_dns_value: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()
        .context("read the DKIM keys to carry")?
    };
    let path = carry_path(data_dir);
    if keys.is_empty() {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(e).context("clear a stale DKIM carry file");
            }
            _ => {}
        }
        return Ok(0);
    }
    let bytes = serde_json::to_vec(&keys).context("encode the carried DKIM keys")?;
    fauna_core::secret_file::write_secret_file_0600(&path, &bytes)
        .context("write the DKIM carry file")?;
    Ok(keys.len())
}

/// The selector of the key a factory reset carried for `domain`, if any — what
/// the add door registers the domain under when the caller names none, so a
/// domain that had rotated comes back on the selector its DNS still publishes.
pub(crate) fn carried_selector(conn: &Connection, domain: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT selector FROM mail_dkim_keys
          WHERE domain = ?1 AND carried_at IS NOT NULL
          ORDER BY created_at DESC, rowid DESC
          LIMIT 1",
        [domain],
        |r| r.get(0),
    )
    .optional()
    .context("read the carried DKIM selector")
}

/// Delete every carried key stamped before `cutoff_ms` — one whose domain was
/// never re-registered. Returns how many went.
pub(crate) fn expire_carried(conn: &Connection, cutoff_ms: i64) -> Result<usize> {
    conn.execute(
        "DELETE FROM mail_dkim_keys WHERE carried_at IS NOT NULL AND carried_at < ?1",
        [cutoff_ms],
    )
    .context("expire carried DKIM keys")
}

/// Unsealed public metadata for one DKIM `(domain, selector)`. Mirrors
/// `fauna_protocol::wrapped_blob::DkimSelectorInfo` on the wire; the sealed
/// key is deliberately *not* part of this row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DkimSelectorRow {
    pub domain: String,
    pub selector: String,
    pub public_dns_value: String,
    /// Epoch-millis when the selector's key was minted.
    pub created_at: i64,
}

impl CacheDb {
    /// The scheduled rotation's mint: a key for `(domain, selector)`. `true`
    /// when this call seated it.
    pub async fn mint_mail_dkim_key(&self, domain: &str, selector: &str) -> Result<bool> {
        mint_selector(&*self.conn().await, domain, selector)
    }

    /// The boot after a factory reset: seat the keys the reset carried
    /// ([`carry_across_factory_reset`]) on this database, stamped `carried_at`,
    /// and remove the file. Returns how many it seated; no file is `Ok(0)`.
    ///
    /// Call it once the deployment keypair is reconciled. A key that does not
    /// open under this database's seed is dropped, never seated: its domain
    /// mints afresh, where a row that will not open would leave the domain's
    /// mail unsigned and fail the next deployment-seed rotation
    /// (`nest_kek::reencrypt_satellites`). With no seed to check against the
    /// file is left for the next boot.
    pub async fn receive_carried_dkim_keys(&self, data_dir: &Path) -> Result<usize> {
        let path = carry_path(data_dir);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e).context("read the DKIM carry file"),
        };
        let keys: Vec<CarriedKey> = match serde_json::from_slice(&bytes) {
            Ok(keys) => keys,
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                return Err(e).context("the DKIM carry file does not parse; removed");
            }
        };
        let mut received = 0;
        {
            let conn = self.conn().await;
            let seed = require_deployment_seed(&conn)?;
            let now = crate::db::now_epoch_millis();
            for key in keys {
                let opens = hex::decode(&key.key_wrapped)
                    .ok()
                    .filter(|wrapped| open_dkim_key(&seed, wrapped).is_ok());
                let Some(wrapped) = opens else {
                    tracing::warn!(
                        target: "mail_dkim",
                        domain = %key.domain,
                        selector = %key.selector,
                        "a carried DKIM key does not open under this nest's deployment seed; \
                         dropped — the domain mints a new key when it is added"
                    );
                    continue;
                };
                received += conn
                    .execute(
                        "INSERT OR IGNORE INTO mail_dkim_keys
                            (domain, selector, alg, key_wrapped, public_dns_value, created_at,
                             carried_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        rusqlite::params![
                            key.domain,
                            key.selector,
                            key.alg,
                            wrapped,
                            key.public_dns_value,
                            key.created_at,
                            now,
                        ],
                    )
                    .context("seat a carried DKIM key")?;
            }
        }
        std::fs::remove_file(&path).context("remove the DKIM carry file")?;
        Ok(received)
    }

    /// Enumerate DKIM selectors as *unsealed* public metadata (no key material
    /// is ever returned here). `domain` restricts to one domain; `None` lists
    /// every selector. A carried key is not a selector until its domain's door
    /// adopts it, so it is never listed. Ordered by `(domain, created_at)` so every reader — the
    /// DNS page, `fauna.setup.status`, the rotation flip and its mint gate —
    /// sees a stable, newest-last list.
    pub async fn list_dkim_selectors(&self, domain: Option<&str>) -> Result<Vec<DkimSelectorRow>> {
        let conn = self.conn().await;
        let mut stmt = conn.prepare(
            "SELECT domain, selector, public_dns_value, created_at
               FROM mail_dkim_keys
              WHERE carried_at IS NULL AND (?1 IS NULL OR domain = ?1)
              ORDER BY domain, created_at, rowid",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![domain], |row| {
                Ok(DkimSelectorRow {
                    domain: row.get(0)?,
                    selector: row.get(1)?,
                    public_dns_value: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Delete a DKIM selector's key (the admin "retire" action). Returns
    /// whether a row existed. Old + new selectors are valid simultaneously
    /// during rotation; retire removes the old one after DNS propagation.
    pub async fn delete_dkim_selector(&self, domain: &str, selector: &str) -> Result<bool> {
        let n = self
            .conn()
            .await
            .execute(
                "DELETE FROM mail_dkim_keys WHERE domain = ?1 AND selector = ?2",
                rusqlite::params![domain, selector],
            )
            .context("retire dkim selector")?;
        Ok(n > 0)
    }
}

#[cfg(test)]
impl CacheDb {
    /// Test seam: seat `(domain, selector)` with a chosen public record,
    /// replacing any key it has. The stored key opens to nothing, so a test
    /// that signs mints its key through a real door instead.
    pub(crate) async fn seat_dkim_selector_for_test(
        &self,
        domain: &str,
        selector: &str,
        public_dns_value: &str,
    ) {
        self.conn()
            .await
            .execute(
                "INSERT OR REPLACE INTO mail_dkim_keys
                    (domain, selector, alg, key_wrapped, public_dns_value, created_at)
                 VALUES (?1, ?2, 'ed25519', x'00', ?3, ?4)",
                rusqlite::params![
                    domain,
                    selector,
                    public_dns_value,
                    crate::db::now_epoch_millis()
                ],
            )
            .expect("seat a dkim selector");
    }
}

/// What the hand-out does with one mail domain's outbound.
enum DomainKey {
    /// The nest holds the active selector's key: it signs.
    Held(SigningKey),
    /// No key that opens — the unit leaves unsigned and is counted.
    Missing,
}

/// The sign site's view of one `fetch_outbound_due` batch: the active mail
/// domains and, for each, the key its active selector signs under.
///
/// Loaded once per batch and dropped with it; the opened keys are zeroized on
/// drop.
pub struct OutboundSigner {
    domains: HashMap<String, DomainKey>,
}

impl Drop for OutboundSigner {
    fn drop(&mut self) {
        for key in self.domains.values_mut() {
            if let DomainKey::Held(key) = key {
                key.priv_key.zeroize();
            }
        }
    }
}

impl OutboundSigner {
    /// Look up every active mail domain's signing key — a read, never a mint.
    ///
    /// The seed is read on `conn` rather than taken from a serving generation,
    /// so a hand-out inside a deployment-seed rotation's hand-off window opens
    /// the re-wrapped rows. A row that will not open is logged and treated as
    /// missing: signing never holds the queue.
    pub fn load(conn: &Connection) -> Result<Self> {
        let rows: Vec<(String, String, Option<(String, Vec<u8>)>)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT d.domain_name,
                            COALESCE(d.dkim_selector, ?1),
                            k.alg,
                            k.key_wrapped
                       FROM mail_domains d
                       LEFT JOIN mail_dkim_keys k
                              ON k.domain = d.domain_name
                             AND k.selector = COALESCE(d.dkim_selector, ?1)
                             AND k.carried_at IS NULL
                      WHERE d.removed_at IS NULL",
                )
                .context("prepare the signing-key read")?;
            stmt.query_map([DEFAULT_SELECTOR], |r| {
                let alg: Option<String> = r.get(2)?;
                let wrapped: Option<Vec<u8>> = r.get(3)?;
                Ok((r.get(0)?, r.get(1)?, alg.zip(wrapped)))
            })?
            .collect::<rusqlite::Result<_>>()
            .context("read the signing keys")?
        };
        let seed = if rows.iter().any(|(_, _, held)| held.is_some()) {
            deployment_seed(conn)?
        } else {
            None
        };
        let mut domains = HashMap::with_capacity(rows.len());
        for (domain, selector, held) in rows {
            let key = match (held, &seed) {
                (Some((alg, wrapped)), Some(seed)) => {
                    match open_signing_key(seed, &domain, &selector, &alg, &wrapped) {
                        Ok(key) => DomainKey::Held(key),
                        Err(e) => {
                            tracing::error!(
                                target: "mail_dkim",
                                domain = %domain,
                                selector = %selector,
                                "the DKIM key will not open; its mail leaves unsigned: {e:#}"
                            );
                            DomainKey::Missing
                        }
                    }
                }
                (Some(_), None) | (None, _) => DomainKey::Missing,
            };
            domains.insert(domain, key);
        }
        Ok(Self { domains })
    }

    /// The signed form of `raw`, or `None` when the unit leaves as it rests.
    ///
    /// Signs only a message that carries exactly one From field naming exactly
    /// one mailbox, whose domain selects a local signing domain
    /// (`mail-multidomain.md` § Signing-key selection at outbound time) for
    /// whose active selector the nest holds the key.
    pub fn sign(&self, raw: &[u8]) -> Option<Vec<u8>> {
        if self.domains.is_empty() || fauna_mail::from_field::from_field_count(raw) != 1 {
            return None;
        }
        let mut mailboxes = fauna_mail::envelope::from_mailboxes(raw);
        if mailboxes.len() != 1 {
            return None;
        }
        let from_domain = mailboxes.remove(0).host;
        let locals: Vec<&str> = self.domains.keys().map(String::as_str).collect();
        let domain =
            fauna_mail::outbound::dkim::select_signing_domain(&from_domain, &locals).ok()?;
        match self.domains.get(domain)? {
            DomainKey::Missing => {
                UNSIGNED_HANDOUTS.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    target: "mail_dkim",
                    domain = %domain,
                    "no DKIM key for the domain's active selector; the message leaves unsigned"
                );
                None
            }
            DomainKey::Held(key) => {
                match fauna_mail::outbound::dkim::sign(raw, std::slice::from_ref(key)) {
                    Ok(signed) => Some(signed),
                    Err(e) => {
                        UNSIGNED_HANDOUTS.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            target: "mail_dkim",
                            domain = %domain,
                            "DKIM sign failed; the message leaves unsigned: {e}"
                        );
                        None
                    }
                }
            }
        }
    }
}

fn open_signing_key(
    deployment_seed: &[u8; 32],
    domain: &str,
    selector: &str,
    alg: &str,
    wrapped: &[u8],
) -> Result<SigningKey> {
    let alg = match alg {
        "ed25519" => SigningAlg::Ed25519,
        "rsa-sha256" => SigningAlg::RsaSha256,
        other => anyhow::bail!("unknown DKIM key algorithm `{other}`"),
    };
    let priv_key = open_dkim_key(deployment_seed, wrapped)
        .context("wrong deployment seed, or a corrupt row")?;
    Ok(SigningKey {
        alg,
        priv_key: priv_key.to_vec(),
        selector: selector.to_string(),
        domain: domain.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        crate::db::migrations::run_migrations(&conn).expect("schema");
        conn
    }

    fn seat_seed(conn: &Connection, seed: &[u8; 32]) {
        let public = ed25519_dalek::SigningKey::from_bytes(seed)
            .verifying_key()
            .to_bytes();
        conn.execute("DELETE FROM nest_keypair", []).unwrap();
        conn.execute(
            "INSERT INTO nest_keypair (id, secret_key, public_key, created_at)
             VALUES (1, ?1, ?2, 0)",
            rusqlite::params![&seed[..], &public[..]],
        )
        .unwrap();
    }

    fn add_domain(conn: &Connection, name: &str, selector: Option<&str>) {
        conn.execute(
            "INSERT INTO mail_domains
                (domain_id, domain_name, is_primary, added_at, dkim_selector,
                 mta_sts_mode, mta_sts_cert_mode)
             VALUES (?1, ?2, 0, 0, ?3, 'testing', 'none')",
            rusqlite::params![&uuid::Uuid::new_v4().as_bytes()[..], name, selector],
        )
        .unwrap();
    }

    fn rows(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM mail_dkim_keys", [], |r| r.get(0))
            .unwrap()
    }

    const RAW: &[u8] = b"From: alice@example.test\r\nSubject: hi\r\n\r\nbody\r\n";

    /// The sign site looks a key up and never mints one.
    #[test]
    fn a_hand_out_never_mints() {
        let conn = db();
        add_domain(&conn, "example.test", None);
        let before = unsigned_handouts();
        let signer = OutboundSigner::load(&conn).expect("load");
        assert!(
            signer.sign(RAW).is_none(),
            "no key, so nothing to sign with"
        );
        assert_eq!(rows(&conn), 0, "a hand-out must never mint a DKIM key");
        assert!(
            unsigned_handouts() > before,
            "a local From leaving unsigned is counted"
        );
    }

    /// The boot rule: every active domain's active selector gets a key, once.
    #[test]
    fn the_boot_mint_seats_keyless_active_selectors_only() {
        let conn = db();
        let seed = [1u8; 32];
        add_domain(&conn, "fresh.test", None);
        add_domain(&conn, "rotated.test", Some("202610"));

        let mut minted = mint_for_keyless_domains(&conn, &seed).expect("mint");
        minted.sort();
        assert_eq!(minted, ["fresh.test", "rotated.test"]);
        assert!(holds(&conn, "fresh.test", "default").unwrap());
        assert!(holds(&conn, "rotated.test", "202610").unwrap());
        assert!(!holds(&conn, "rotated.test", "default").unwrap());
        seat_seed(&conn, &seed);
        assert!(
            !mint_selector(&conn, "fresh.test", "default").unwrap(),
            "no door mints over a key that exists"
        );

        assert!(
            mint_for_keyless_domains(&conn, &seed).unwrap().is_empty(),
            "a second boot is first-write-wins — the published record stays put"
        );
        assert_eq!(rows(&conn), 2);
    }

    /// The row is ciphertext under the deployment seed, and a deliberate door
    /// seals under the seed the database holds.
    #[test]
    fn the_key_is_sealed_under_the_seed_the_database_holds() {
        let conn = db();
        let seed = [3u8; 32];
        seat_seed(&conn, &seed);
        add_domain(&conn, "example.test", None);
        assert!(mint_selector(&conn, "example.test", "default").unwrap());

        let wrapped: Vec<u8> = conn
            .query_row("SELECT key_wrapped FROM mail_dkim_keys", [], |r| r.get(0))
            .unwrap();
        let opened = open_dkim_key(&seed, &wrapped).expect("opens under the database's seed");
        assert!(open_dkim_key(&[4u8; 32], &wrapped).is_err());
        assert!(
            !wrapped
                .windows(opened.len())
                .any(|w| w == opened.as_slice()),
            "the plaintext key appears verbatim inside the stored blob"
        );
    }

    /// A key that will not open ships the mail unsigned rather than failing
    /// the batch.
    #[test]
    fn a_key_that_will_not_open_leaves_the_mail_unsigned() {
        let conn = db();
        seat_seed(&conn, &[3u8; 32]);
        add_domain(&conn, "example.test", None);
        mint(&conn, &[9u8; 32], "example.test", "default").unwrap();
        let signer = OutboundSigner::load(&conn).expect("load does not fail the batch");
        assert!(signer.sign(RAW).is_none());
    }

    /// A subdomain From signs under the closest local parent; a foreign From
    /// is not a missing key.
    #[test]
    fn the_signing_domain_is_selected_from_the_nests_own_rows() {
        let conn = db();
        seat_seed(&conn, &[3u8; 32]);
        add_domain(&conn, "example.test", None);
        mint_selector(&conn, "example.test", "default").unwrap();
        let signer = OutboundSigner::load(&conn).unwrap();

        let signed = signer
            .sign(b"From: a@mail.example.test\r\n\r\nbody\r\n")
            .expect("a subdomain From signs under its local parent");
        let header = String::from_utf8_lossy(&signed).replace([' ', '\r', '\n', '\t'], "");
        assert!(header.contains("d=example.test;"), "{header}");
        assert!(header.contains("s=default;"), "{header}");

        let before = unsigned_handouts();
        assert!(
            signer
                .sign(b"From: a@elsewhere.test\r\n\r\nbody\r\n")
                .is_none()
        );
        assert_eq!(
            unsigned_handouts(),
            before,
            "a foreign From is not a missing key"
        );
    }

    /// A deployment-seed rotation carries the row — the `nest_kek::SATELLITES`
    /// registration, pinned end to end.
    #[test]
    fn a_deployment_seed_rotation_keeps_the_same_key() {
        let conn = db();
        add_domain(&conn, "example.test", None);
        mint(&conn, &[1u8; 32], "example.test", "default").unwrap();
        let wrapped = |conn: &Connection| -> Vec<u8> {
            conn.query_row("SELECT key_wrapped FROM mail_dkim_keys", [], |r| r.get(0))
                .unwrap()
        };
        let before = open_dkim_key(&[1u8; 32], &wrapped(&conn)).unwrap();
        crate::nest_kek::reencrypt_satellites(&conn, &[1u8; 32], &[2u8; 32]).expect("rotate");
        assert_eq!(open_dkim_key(&[2u8; 32], &wrapped(&conn)).unwrap(), before);
    }

    /// The deployment-seed rotation's hand-off window
    /// (`key-material-hierarchy.md` § Room-read keypair → *When it mints*;
    /// `room_read_key::tests::rotation_window` is the model): the ceremony has
    /// committed while the outgoing generation still serves.
    mod rotation_window {
        use std::sync::Arc;

        use zeroize::Zeroizing;

        use crate::db::CacheDb;
        use crate::mail_dkim_key::OutboundSigner;

        fn seed(byte: u8) -> Zeroizing<[u8; 32]> {
            Zeroizing::new([byte; 32])
        }

        async fn opens(db: &Arc<CacheDb>) -> bool {
            OutboundSigner::load(&*db.conn().await)
                .unwrap()
                .sign(super::RAW)
                .is_some()
        }

        /// A domain added inside the window is minted by a door that reads the
        /// seed from the database, so its key is sealed under the successor;
        /// a hand-out in the window mints nothing; and the next rotation
        /// commits and carries the key.
        #[tokio::test]
        async fn a_domain_added_in_the_rotation_window_seals_under_the_successor_seed() {
            let (a, b, c) = (seed(0xa1), seed(0xb2), seed(0xc3));
            let db = Arc::new(CacheDb::open_in_memory().unwrap());
            crate::test_support::seat_deployment_seed(&db, &a).await;
            db.rotate_deployment_seed(&a, &b).await.unwrap().unwrap();

            assert!(!opens(&db).await, "no domain, so nothing to sign");
            db.add_mail_domain("example.test", true, "testing", "none", None, None)
                .await
                .unwrap();
            assert!(
                opens(&db).await,
                "the door sealed under the seed the database holds, not a retired copy"
            );
            let before = db.list_dkim_selectors(Some("example.test")).await.unwrap();

            let next = db
                .rotate_deployment_seed(&b, &c)
                .await
                .unwrap()
                .expect("the next rotation commits — nothing wedges the satellite walk");
            assert!(next.satellites_rekeyed >= 1);
            assert!(opens(&db).await);
            assert_eq!(
                db.list_dkim_selectors(Some("example.test")).await.unwrap(),
                before,
                "and the published record stays put"
            );
        }
    }
}
