//! The **nest-internal key-encryption key** family — one audit point for every
//! secret the nest wraps under its own deployment seed.
//!
//! `docs/goal/architecture/key-material-hierarchy.md` § Audience: deployment
//! infrastructure → *Nest-internal key-encryption key* describes this family as
//! one entry with siblings: same root (the deployment Ed25519 signing seed),
//! same AEAD (`fauna_core::crypto::{encrypt,decrypt}_backup_chunk`), same wire
//! shape, each member domain-separated by **its own BLAKE3 context string**
//! (rule #3). This module is that description made structural: the context
//! strings live here, and so does the one walk that re-keys every member.
//!
//! # Why the walk lives here and not in the plane modules
//!
//! Each member's typed wrapper lives with its plane (`nostr/key_crypto.rs`,
//! `activitypub/key_crypto.rs`, `atproto_authoring_key.rs`), and every one of
//! those planes is behind a **cargo feature**. The deployment-seed rotation
//! ceremony must re-encrypt *all* of them in its transaction or the skipped ones
//! become permanently undecryptable — "user-data loss of the deposited-nsec
//! class" (`nest/box-recovery.md` § The ceremony, step 4).
//!
//! ⚠ A feature-gated walk would satisfy that only for the flavor it was built
//! with, and the flavors genuinely differ: the Docker image ships
//! `{bluesky, nostr, activitypub}` while the Windows `FaunaNest` service exe
//! builds none of them (`merge-gate-check.md` § Feature scope — measured by the 110th
//! and 112th passes). A DB written by the first and rotated by the second would
//! lose every plane the second cannot see, silently, with both binaries green.
//! So the walk keys off **what is in the database**, not what is in the build:
//! it skips a table that does not exist and re-keys every table that does. The
//! plane modules keep their typed wrappers and simply derive from the constants
//! here, so there is exactly one list and it cannot drift from the enforcement.
//!
//! # The boot step that seats the single-row members
//!
//! Three members are one row per deployment (for the issuer key, one active
//! signer in a set) that the nest mints for itself, with no user act behind
//! them: the room-read keypair, the OAuth issuer key's active signer and the
//! OAuth session secret. [`mint_at_boot`] seats all three before a serving
//! generation answers anything, and outside an admin's rotation it is the only
//! door that writes them (`key-material-hierarchy.md` § Audience: deployment
//! infrastructure → *Room-read keypair* → *When it mints*); every read of those
//! rows is a lookup that answers [`NotProvisioned`] when the row is absent.
//!
//! The reason is the rotation's own hand-off window (`nest/box-recovery.md`
//! § Deployment-seed rotation → *The bounded hand-off window*): after the
//! transaction commits, the outgoing serving generation still holds the retired
//! seed in memory, and a row sealed under that copy never opens again — the
//! walk below then refuses it on every later rotation. The boot step reads the
//! seed out of `nest_keypair` with [`deployment_seed`] on the connection it
//! inserts on, under the database guard the rotation also holds for its whole
//! transaction, so it seals under the seed the database holds at that instant.
//! A door that mints on purpose later reads the seed the same way.
//!
//! The other members have no boot moment, so they are not part of the boot
//! step. The per-account ones — the deposited nsec, the NIP-46 bunker signer,
//! an ActivityPub account's key and the ATProto authoring sub-key — mint on a
//! user's act, and an account created tomorrow has no row today. The
//! ActivityPub instance actor is a single row, but it needs the nest's domain,
//! so a domainless nest cannot mint it at boot and its first use after a later
//! domain claim does. Every one of those doors reads the seed with
//! [`require_deployment_seed`] on the connection it inserts on, holding the
//! guard from that read through the insert.

use std::sync::Arc;

use anyhow::{Context, Result};
use fauna_core::crypto::BackupKey;
use rusqlite::{Connection, OptionalExtension as _};
use zeroize::Zeroizing;

use crate::db::CacheDb;

/// Deposited Nostr secp256k1 identity privkeys (`nostr_accounts`).
pub const NOSTR_NSEC_CONTEXT: &str = "fauna nostr key encryption 2026-03-19";

/// Per-account NIP-46 bunker signer keypairs (`nostr_bunker_signers`).
pub const BUNKER_SIGNER_CONTEXT: &str = "fauna nostr bunker signer key encryption 2026-07-20";

/// ActivityPub RSA privkeys — both the per-account keys (`ap_accounts`) and the
/// single instance actor (`ap_instance_actor`), which share one context.
pub const ACTIVITYPUB_RSA_CONTEXT: &str = "fauna activitypub key encryption 2026-03-20";

/// The ATProto D10 delegated authoring sub-key `K` (`atproto_authoring_keys`).
pub const ATPROTO_AUTHORING_CONTEXT: &str = "fauna atproto authoring key encryption 2026-07-23";

/// The nest-held OAuth issuer ES256 signing keys (`oauth_issuer_keys`).
///
/// `key-material-hierarchy.md` § Audience: deployment infrastructure →
/// *Issuer signing key* requires its **own** context string, dated at
/// introduction and never a reuse of the nsec / K / bunker contexts (rule #3).
///
/// ⚠ Unlike every sibling above, this member's plane is **not** behind a cargo
/// feature: `authorization-server.md` § The issuer rules that the AS "is up
/// whenever the nest is up". The walk below keys off the database rather than
/// the build, so that difference costs it nothing — but a future edit that
/// gates the plane would silently un-rule the goal doc, and this is the note
/// that says so.
pub const OAUTH_ISSUER_CONTEXT: &str = "fauna oauth issuer key encryption 2026-09-08";

/// The nest's OAuth-plane **refresh-token signing secret** (HS256), in
/// `oauth_session_secret`.
///
/// `key-material-hierarchy.md` § Audience: deployment infrastructure →
/// *OAuth refresh-token signing secret* — the second signer of the
/// nest-hosted authorization server, beside the ES256 issuer key above. Its
/// own dated context per rule #3, never a reuse of the issuer's: the two are
/// different algorithms serving different token classes, and one context
/// opening both would make "which key signed this" a question the seal cannot
/// answer.
///
/// ⚠ Not behind a cargo feature, for the same reason
/// [`OAUTH_ISSUER_CONTEXT`] is not: the AS is up whenever the nest is up, so
/// its second signer is too.
pub const OAUTH_SESSION_CONTEXT: &str = "fauna oauth session secret encryption 2026-09-09";

/// The nest's X-Wing **room-read** reception keypair ikm
/// (`nest_room_read_key`).
///
/// `key-material-hierarchy.md` § Audience: deployment infrastructure →
/// *Room-read keypair* — the wrap target that makes this nest a readable
/// member of the community rooms it homes. Its own dated context per rule #3,
/// never a reuse of a sibling's.
///
/// ⚠ **This member is the reason the plane is a satellite at all.** The key's
/// public half rests in other people's wraps — an owner or admin device seals
/// a room's generation key to it at admission — so unlike every sibling here,
/// losing or *changing* this secret is not a local inconvenience: it makes
/// rows on other members' planes unopenable, and no ceremony can rewrite
/// them. Sealing a stable random ikm under a seed-derived KEK is what lets a
/// deployment-seed rotation re-key the row (below) while the reception
/// **public key stays put**. Deriving the keypair from the seed directly
/// would have been simpler and would have silently broken every community
/// room on the first rotation.
pub const ROOM_READ_CONTEXT: &str = "fauna nest room read key encryption 2026-09-09";

/// The media proxy's **playback-ticket signing secret** (HMAC-SHA-256), in
/// `media_ticket_secret`.
///
/// `key-material-hierarchy.md` § Audience: deployment infrastructure →
/// *Media playback-ticket secret* — the proxy's second credential beside the
/// bearer. Its own dated context per rule #3, never a reuse of the OAuth
/// session secret's: one key, one purpose, so a leaked ticket is never a
/// refresh-token forgery oracle and the seal can always say which key it holds.
pub const MEDIA_TICKET_CONTEXT: &str = "fauna media ticket secret encryption 2026-10-02";

/// A first-party in-process bridge leg's **X25519 secret**, in
/// `first_party_bridge_keys` — today the Nostr DM leg's, the key its rooms'
/// `bridge_x25519` names and the user's outbound items are sealed to
/// (`ui/conversations.md` § Where logic lives → *The `Bridged` adapter*,
/// ruling 2's honest exception). Minted on the leg's first use, never at boot:
/// a nest with no Nostr account linked never needs one.
pub const FIRST_PARTY_BRIDGE_CONTEXT: &str = "fauna first-party bridge key encryption 2026-10-03";

/// A mail domain's **DKIM signing key**, one per (domain, selector), in
/// `mail_dkim_keys` (`key-material-hierarchy.md` § Audience: deployment
/// infrastructure → *The oracle* → *The DKIM class is the outbound spool's own
/// door*). The deployment's identity to peer MTAs: the public half is
/// published in DNS, so a rotation that stranded the row would leave a
/// published record nothing can sign under. Minted at boot for the mail
/// domains that exist and by the doors that add one or rotate
/// ([`crate::mail_dkim_key`]).
pub const MAIL_DKIM_CONTEXT: &str = "fauna mail dkim key encryption 2026-10-03";

/// Derive one family member's key-encryption key from the deployment seed.
///
/// The single derivation point every member routes through, so a context string
/// can never be spelled two ways in two places.
pub fn derive(context: &str, deployment_seed: &[u8; 32]) -> BackupKey {
    BackupKey::from_bytes(blake3::derive_key(context, deployment_seed))
}

/// Wrap a 32-byte secret under a family member's derived key-encryption key.
/// The construction every fixed-size plane wrapper shares
/// (`nostr/key_crypto.rs`, `atproto_authoring_key.rs`) — callers keep their
/// own typed, documented function name over their own context constant.
/// `activitypub/key_crypto.rs`'s RSA keys are variable-length DER, not this
/// shape; it calls [`derive`] + `encrypt_backup_chunk` directly.
pub fn wrap_32(context: &str, deployment_seed: &[u8; 32], secret: &[u8; 32]) -> Result<Vec<u8>> {
    let key = derive(context, deployment_seed);
    fauna_core::crypto::encrypt_backup_chunk(&key, secret)
}

/// Unwrap a 32-byte secret wrapped by [`wrap_32`]. Errors — rather than
/// silently truncating or panicking — if the decrypted plaintext isn't
/// exactly 32 bytes (a corrupted ciphertext or the wrong context/seed).
pub fn unwrap_32(context: &str, deployment_seed: &[u8; 32], ciphertext: &[u8]) -> Result<[u8; 32]> {
    let key = derive(context, deployment_seed);
    let plaintext = fauna_core::crypto::decrypt_backup_chunk(&key, ciphertext)?;
    plaintext.try_into().map_err(|v: Vec<u8>| {
        anyhow::anyhow!(
            "decrypted secret has wrong size: expected 32, got {}",
            v.len()
        )
    })
}

/// The deployment seed `nest_keypair` holds, read on the caller's connection —
/// so under whatever database guard, or inside whatever transaction, the
/// caller already holds.
///
/// `None` when the nest holds no deployment keypair at all. A door that seals a
/// row on purpose reads its seed here rather than taking a serving generation's
/// copy: that copy is a snapshot, and once a deployment-seed rotation commits,
/// the outgoing generation's snapshot is the retired seed.
pub fn deployment_seed(conn: &Connection) -> Result<Option<Zeroizing<[u8; 32]>>> {
    let secret: Option<Zeroizing<Vec<u8>>> = conn
        .query_row(
            "SELECT secret_key FROM nest_keypair WHERE id = 1",
            [],
            |r| r.get::<_, Vec<u8>>(0),
        )
        .optional()
        .context("read the deployment seed from nest_keypair")?
        .map(Zeroizing::new);
    let Some(secret) = secret else {
        return Ok(None);
    };
    let seed = <[u8; 32]>::try_from(secret.as_slice()).map_err(|_| {
        anyhow::anyhow!(
            "the nest_keypair secret is {} bytes, not a 32-byte deployment seed",
            secret.len()
        )
    })?;
    Ok(Some(Zeroizing::new(seed)))
}

/// [`deployment_seed`] for a door that mints: a nest with no deployment keypair
/// has nothing to seal under, and says so.
pub fn require_deployment_seed(conn: &Connection) -> Result<Zeroizing<[u8; 32]>> {
    deployment_seed(conn)?.context("this nest holds no deployment keypair to seal under")
}

/// A single-row member of this family is absent: the boot step
/// ([`mint_at_boot`]) has not run, or failed for that member (`start_server`
/// logs why). Carries the member's name.
///
/// A distinct type so a consumer can tell "this nest has no such key" from "the
/// row will not open under the seed I hold" — and, above all, so an absent row
/// is an answer a read returns rather than a gap it fills.
#[derive(Debug, thiserror::Error)]
#[error(
    "the {0} has not been minted: it is minted at boot, before the nest serves, and never by a \
     read"
)]
pub struct NotProvisioned(pub &'static str);

/// What [`mint_at_boot`] seated, one result per member, so a member whose row
/// will not open does not keep the others from being seated.
pub struct BootMint {
    /// The room-read reception **public** key.
    pub room_read_public_key: Result<Vec<u8>>,
    /// The `kid` of the OAuth issuer key set's active signer.
    pub issuer_kid: Result<String>,
    /// The OAuth refresh-token signing secret — seated, never handed out.
    pub session_secret: Result<()>,
    /// The media proxy's playback-ticket secret — seated, never handed out.
    pub media_ticket_secret: Result<()>,
    /// The mail domains a DKIM key was minted for: every active domain whose
    /// active selector had no key.
    pub mail_dkim_keys: Result<Vec<String>>,
}

impl BootMint {
    /// Every member that failed, by name — what `start_server` logs.
    pub fn failures(&self) -> Vec<(&'static str, &anyhow::Error)> {
        [
            (
                "room-read keypair",
                self.room_read_public_key.as_ref().err(),
            ),
            ("OAuth issuer signing key", self.issuer_kid.as_ref().err()),
            ("OAuth session secret", self.session_secret.as_ref().err()),
            (
                "media playback-ticket secret",
                self.media_ticket_secret.as_ref().err(),
            ),
            ("mail DKIM keys", self.mail_dkim_keys.as_ref().err()),
        ]
        .into_iter()
        .filter_map(|(member, failure)| failure.map(|e| (member, e)))
        .collect()
    }
}

/// Seat every single-row member the nest mints for itself — the room-read
/// keypair, the OAuth issuer key set's active signer, the OAuth session
/// secret and the media playback-ticket secret — and a DKIM key for every
/// mail domain that has none. The boot step, and the only caller of each
/// single-row member's `mint_if_absent`.
///
/// `start_server` calls it before the serving generation it builds answers
/// anything (the module docs say why no other caller may exist). The seed is
/// read from `nest_keypair` under the same connection guard every insert runs
/// under, so each row is sealed under the seed the database holds right then.
/// Every member is mint-if-absent and first-write-wins, so a restart, a
/// serving-generation re-entry and a nest upgrading into the rule all pass
/// through harmlessly.
///
/// `Ok(None)` when the nest holds no deployment keypair: there is nothing to
/// seal under, and every consumer then answers [`NotProvisioned`].
pub async fn mint_at_boot(db: &Arc<CacheDb>) -> Result<Option<BootMint>> {
    let db = db.clone();
    tokio::task::spawn_blocking(move || -> Result<Option<BootMint>> {
        let conn = db.conn_blocking();
        let Some(seed) = deployment_seed(&conn)? else {
            return Ok(None);
        };
        Ok(Some(BootMint {
            room_read_public_key: crate::room_read_key::mint_if_absent(&conn, &seed)
                .map(|key| key.public_key),
            issuer_kid: crate::oauth_issuer_key::mint_if_absent(&conn, &seed),
            session_secret: crate::oauth_session_secret::mint_if_absent(&conn, &seed),
            media_ticket_secret: crate::media_ticket_secret::mint_if_absent(&conn, &seed),
            mail_dkim_keys: crate::mail_dkim_key::mint_for_keyless_domains(&conn, &seed),
        }))
    })
    .await
    .context("the boot mint task")?
}

/// One re-keyable column: a table, the column holding the ciphertext, and the
/// context whose derived key opens it.
struct Satellite {
    table: &'static str,
    column: &'static str,
    context: &'static str,
}

/// **Every** ciphertext in the nest DB wrapped under this family.
///
/// ⚠ Adding a member to the family means adding a row here in the same change.
/// A wrapper with no row keeps working until the day someone rotates, and then
/// its secret is gone — which is precisely the failure the ATProto authoring
/// sub-key was heading for: it was a ratified sibling of this family
/// (`key-material-hierarchy.md` § Nest-internal key-encryption key → *Sibling —
/// ATProto authoring sub-key*) that the ceremony's own enumeration had missed.
const SATELLITES: &[Satellite] = &[
    Satellite {
        table: "nostr_accounts",
        column: "encrypted_privkey",
        context: NOSTR_NSEC_CONTEXT,
    },
    Satellite {
        table: "nostr_bunker_signers",
        column: "encrypted_privkey",
        context: BUNKER_SIGNER_CONTEXT,
    },
    Satellite {
        table: "ap_accounts",
        column: "encrypted_privkey",
        context: ACTIVITYPUB_RSA_CONTEXT,
    },
    Satellite {
        table: "ap_instance_actor",
        column: "encrypted_privkey",
        context: ACTIVITYPUB_RSA_CONTEXT,
    },
    Satellite {
        table: "atproto_authoring_keys",
        column: "k_secret_wrapped",
        context: ATPROTO_AUTHORING_CONTEXT,
    },
    Satellite {
        table: "oauth_issuer_keys",
        column: "secret_wrapped",
        context: OAUTH_ISSUER_CONTEXT,
    },
    Satellite {
        table: "nest_room_read_key",
        column: "ikm_wrapped",
        context: ROOM_READ_CONTEXT,
    },
    Satellite {
        table: "oauth_session_secret",
        column: "secret_wrapped",
        context: OAUTH_SESSION_CONTEXT,
    },
    Satellite {
        table: "media_ticket_secret",
        column: "secret_wrapped",
        context: MEDIA_TICKET_CONTEXT,
    },
    Satellite {
        table: "first_party_bridge_keys",
        column: "secret_wrapped",
        context: FIRST_PARTY_BRIDGE_CONTEXT,
    },
    Satellite {
        table: "mail_dkim_keys",
        column: "key_wrapped",
        context: MAIL_DKIM_CONTEXT,
    },
];

/// Re-encrypt every satellite ciphertext from `old_seed`'s derived keys to
/// `new_seed`'s, on an open transaction.
///
/// Returns the number of rows rewritten (for the ceremony's audit line).
///
/// **Caller contract:** run this inside the rotation transaction, while the box
/// holds both roots — the only instant it ever does. Every row is rewritten or
/// none is; a partial re-key is exactly the data loss this exists to prevent.
///
/// A row whose ciphertext does not open under `old_seed` is a **hard error**, not
/// a skip: it means either the row was written under a key this box no longer
/// knows (so the rotation would strand it) or the caller passed the wrong seed.
/// Either way the transaction must roll back rather than write a second layer of
/// unopenable bytes over the first.
pub fn reencrypt_satellites(
    tx: &Connection,
    old_seed: &[u8; 32],
    new_seed: &[u8; 32],
) -> Result<usize> {
    let mut rewritten = 0usize;
    for sat in SATELLITES {
        if !table_exists(tx, sat.table)? {
            continue;
        }
        let old_key = derive(sat.context, old_seed);
        let new_key = derive(sat.context, new_seed);

        // Read the whole table's ciphertexts first: rusqlite cannot hold a live
        // statement open across the UPDATEs that rewrite the rows it is walking.
        // These tables are deployment-scale (accounts, not messages), so the
        // in-memory set is small; `rowid` addresses the row uniformly whatever
        // shape the table's declared primary key has.
        let rows: Vec<(i64, Option<Vec<u8>>)> = {
            let sql = format!("SELECT rowid, {} FROM {}", sat.column, sat.table);
            let mut stmt = tx.prepare(&sql).with_context(|| {
                format!("prepare satellite read for {}.{}", sat.table, sat.column)
            })?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .with_context(|| format!("read satellite rows from {}", sat.table))?
        };

        let update_sql = format!(
            "UPDATE {} SET {} = ?1 WHERE rowid = ?2",
            sat.table, sat.column
        );
        for (rowid, ciphertext) in rows {
            // A NULL column is a legitimate state, not a missing key: an account
            // in a remote-signing mode deposits no nsec at all.
            let Some(ciphertext) = ciphertext else {
                continue;
            };
            let plaintext = fauna_core::crypto::decrypt_backup_chunk(&old_key, &ciphertext)
                .with_context(|| {
                    format!(
                        "{}.{} rowid {rowid} does not open under the superseded deployment seed \
                         — refusing to rotate rather than strand it",
                        sat.table, sat.column
                    )
                })?;
            let resealed = fauna_core::crypto::encrypt_backup_chunk(&new_key, &plaintext)
                .with_context(|| format!("re-seal {}.{} rowid {rowid}", sat.table, sat.column))?;
            tx.execute(&update_sql, rusqlite::params![resealed, rowid])
                .with_context(|| {
                    format!("write re-sealed {}.{} rowid {rowid}", sat.table, sat.column)
                })?;
            rewritten += 1;
        }
    }
    Ok(rewritten)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            rusqlite::params![table],
            |r| r.get(0),
        )
        .context("probe sqlite_master for a satellite table")?;
    Ok(n > 0)
}

/// The boot step, driven through the deployment-seed rotation's hand-off window
/// for every member at once — `room_read_key::tests::rotation_window` drives
/// the room-read member alone, and `oauth_issuer_handlers::tests::rotation_window`
/// the doors that mint on purpose.
#[cfg(test)]
mod boot_mint_tests {
    use std::sync::Arc;

    use zeroize::Zeroizing;

    use crate::db::CacheDb;

    fn seed(byte: u8) -> Zeroizing<[u8; 32]> {
        Zeroizing::new([byte; 32])
    }

    async fn nest_with_deployment_seed(seed: &[u8; 32]) -> Arc<CacheDb> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let public = ed25519_dalek::SigningKey::from_bytes(seed)
            .verifying_key()
            .to_bytes();
        db.set_nest_keypair(seed, &public).await.unwrap();
        db
    }

    /// Whether each member opens under `seed`: the room-read keypair, the
    /// issuer's active signer, the session secret and the media playback-ticket
    /// secret, in that order.
    async fn members_open_under(db: &Arc<CacheDb>, seed: &[u8; 32]) -> [bool; 4] {
        let (db, seed) = (db.clone(), *seed);
        tokio::task::spawn_blocking(move || {
            let conn = db.conn_blocking();
            [
                crate::room_read_key::room_read_key(&conn, &seed).is_ok(),
                crate::oauth_issuer_key::active_signer(&conn, &seed).is_ok(),
                crate::oauth_session_secret::session_secret(&conn, &seed).is_ok(),
                crate::media_ticket_secret::ticket_secret(&conn, &seed).is_ok(),
            ]
        })
        .await
        .unwrap()
    }

    fn failures(minted: &super::BootMint) -> Vec<String> {
        minted
            .failures()
            .into_iter()
            .map(|(member, e)| format!("{member}: {e:#}"))
            .collect()
    }

    /// The review's shape, for every member at once: no row exists when the
    /// ceremony runs, the successor generation's boot step seats all four
    /// under the seed the database holds, a second boot keeps them, and the
    /// next rotation commits and carries them.
    #[tokio::test]
    async fn the_boot_step_seats_every_member_under_the_seed_the_database_holds() {
        let (a, b, c) = (seed(0xa1), seed(0xb2), seed(0xc3));
        let db = nest_with_deployment_seed(&a).await;
        db.rotate_deployment_seed(&a, &b).await.unwrap().unwrap();

        let minted = super::mint_at_boot(&db)
            .await
            .unwrap()
            .expect("the rotated nest still holds a deployment keypair");
        assert!(
            failures(&minted).is_empty(),
            "every member seats: {:?}",
            failures(&minted)
        );
        assert_eq!(
            members_open_under(&db, &b).await,
            [true; 4],
            "every member is sealed under the seed the database holds"
        );
        assert_eq!(
            members_open_under(&db, &a).await,
            [false; 4],
            "and none under the retired copy"
        );

        let kid = minted.issuer_kid.unwrap();
        let again = super::mint_at_boot(&db).await.unwrap().unwrap();
        assert_eq!(
            again.issuer_kid.unwrap(),
            kid,
            "a second boot is first-write-wins — the signer stays put"
        );

        let next = db
            .rotate_deployment_seed(&b, &c)
            .await
            .unwrap()
            .expect("the next rotation commits");
        assert!(
            next.satellites_rekeyed >= 4,
            "the four rows ride the rotation"
        );
        assert_eq!(members_open_under(&db, &c).await, [true; 4]);
    }

    /// A member whose row will not open does not keep the others from being
    /// seated: each member's failure is its own, and the next start tries
    /// again.
    #[tokio::test]
    async fn a_member_that_will_not_open_does_not_keep_the_others_from_being_seated() {
        let (a, stranger) = (seed(0xa1), [0x99u8; 32]);
        let db = nest_with_deployment_seed(&a).await;
        // A session secret sealed under a seed this nest never held.
        let db2 = db.clone();
        tokio::task::spawn_blocking(move || {
            crate::oauth_session_secret::mint_if_absent(&db2.conn_blocking(), &stranger).unwrap()
        })
        .await
        .unwrap();

        let minted = super::mint_at_boot(&db).await.unwrap().unwrap();
        assert!(
            minted.session_secret.is_err(),
            "the stranded row refuses rather than being minted over"
        );
        assert_eq!(
            members_open_under(&db, &a).await,
            [true, true, false, true],
            "and the other three members seat regardless"
        );
    }

    /// A nest with no deployment keypair has nothing to seal under, and says so
    /// rather than inventing a seed.
    #[tokio::test]
    async fn a_nest_without_a_deployment_keypair_mints_nothing() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let db2 = db.clone();
        tokio::task::spawn_blocking(move || {
            db2.conn_blocking()
                .execute("DELETE FROM nest_keypair", [])
                .unwrap()
        })
        .await
        .unwrap();

        assert!(super::mint_at_boot(&db).await.unwrap().is_none());

        let db2 = db.clone();
        let rows: i64 = tokio::task::spawn_blocking(move || {
            let conn = db2.conn_blocking();
            [
                "nest_room_read_key",
                "oauth_issuer_keys",
                "oauth_session_secret",
                "media_ticket_secret",
            ]
            .iter()
            .map(|table| {
                conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap()
            })
            .sum()
        })
        .await
        .unwrap();
        assert_eq!(rows, 0);
    }

    /// No read path may mint, and visibility enforces that only as far as a
    /// member's `mint_if_absent` is `pub(crate)`: a future consumer could call
    /// it — or the public boot step — "for convenience" and reopen the rotation
    /// window. The walk covers the whole nest source tree rather than a file
    /// list, the same discipline as the serving-generation spawn ratchet
    /// (`state.rs`). It matches the bare names — an import, a call, even a
    /// comment — so it errs toward a false red rather than a miss. This module
    /// and the four member modules are skipped: they define the doors and pin
    /// them.
    #[test]
    fn the_boot_step_alone_mints_the_members_and_start_server_alone_boots() {
        const OWNERS: [&str; 5] = [
            "nest_kek.rs",
            "room_read_key.rs",
            "oauth_issuer_key.rs",
            "oauth_session_secret.rs",
            "media_ticket_secret.rs",
        ];
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let (mut member_mints, mut boot_steps) = (Vec::new(), Vec::new());
        let mut walked = 0usize;
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                walked += 1;
                if OWNERS.iter().any(|owner| path.ends_with(owner)) {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                let rel = path.strip_prefix(&src).unwrap().display().to_string();
                if text.contains("mint_if_absent") {
                    member_mints.push(rel.clone());
                }
                if text.contains("mint_at_boot") {
                    boot_steps.push(rel);
                }
            }
        }
        assert!(
            walked > 100,
            "the walk found only {walked} files — a moved `src`?"
        );
        member_mints.sort();
        boot_steps.sort();
        assert!(
            member_mints.is_empty(),
            "a member's mint is called only by the boot step, yet {member_mints:?} names one — \
             a mint outside boot can seal a single-row satellite under a seed a rotation has \
             just retired"
        );
        assert_eq!(
            boot_steps,
            vec!["lib.rs".to_string(), "test_support.rs".to_string()],
            "the boot step runs only from `start_server` and from the one test helper that \
             stands a nest up the same way — a read path that boots can seal under a seed a \
             rotation has just retired"
        );
    }

    /// Every production door that seals a member with no boot moment reads its
    /// seed from the database, and a new such door is a deliberate act. Those
    /// members' wrappers take the seed as an argument, so nothing in the type
    /// stops a caller passing a serving generation's copy — which inside a
    /// rotation's hand-off window is the retired seed. The walk takes each
    /// file's text up to its `#[cfg(test)] mod`, finds every call of a member's
    /// seal, requires each file making one to read the seed with
    /// [`super::require_deployment_seed`], and pins the set of those files. A
    /// new file on the list is a new minting door: read the seed on the
    /// connection it inserts on, then add the file here.
    #[test]
    fn every_door_sealing_a_member_with_no_boot_moment_reads_the_seed_from_the_database() {
        const SEALS: [&str; 5] = [
            "encrypt_nostr_privkey(",
            "encrypt_bunker_signer_privkey(",
            "encrypt_rsa_privkey(",
            "encrypt_authoring_key_secret(",
            "seal_dkim_key(",
        ];
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut doors = Vec::new();
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                let production = production_text(&text);
                let seals = production.lines().any(|line| {
                    SEALS
                        .iter()
                        .any(|seal| line.contains(seal) && !line.contains(&format!("fn {seal}")))
                });
                if !seals {
                    continue;
                }
                let rel = path
                    .strip_prefix(&src)
                    .unwrap()
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                assert!(
                    production.contains("require_deployment_seed("),
                    "{rel} seals a member with no boot moment but never reads the seed from the \
                     database — inside a rotation's hand-off window a serving generation's copy \
                     is the retired seed"
                );
                doors.push(rel);
            }
        }
        doors.sort();
        assert_eq!(
            doors,
            [
                "activitypub/bridge_provider.rs",
                "activitypub/instance_actor.rs",
                "atproto_authoring_key.rs",
                "mail_dkim_key.rs",
                "nostr/bridge_provider.rs",
                "nostr/bunker.rs",
            ],
            "the doors sealing a member with no boot moment changed — a new one reads the seed \
             on the connection it inserts on (`key-material-hierarchy.md` § Room-read keypair → \
             When it mints) before it joins this list"
        );
    }

    /// `text` up to its first `#[cfg(test)]` that gates a module — where every
    /// file in this crate keeps its tests.
    fn production_text(text: &str) -> &str {
        let mut offset = 0;
        let mut lines = text.split_inclusive('\n').peekable();
        while let Some(line) = lines.next() {
            if line.trim() == "#[cfg(test)]"
                && lines
                    .peek()
                    .is_some_and(|next| next.trim_start().starts_with("mod "))
            {
                return &text[..offset];
            }
            offset += line.len();
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The context strings are FROZEN.** Each one is the sole input (beside the
    /// seed) to the key that opens live at-rest ciphertext, so changing a single
    /// character silently makes every existing secret of that family
    /// undecryptable on the next upgrade — deposited nsecs, bunker signers,
    /// ActivityPub RSA keys, ATProto authoring sub-keys, OAuth issuer signing
    /// keys. Nothing re-wraps them
    /// except the rotation ceremony, and it derives from these same constants, so
    /// there is no self-healing path.
    ///
    /// Round-trip tests cannot catch this: a typo'd constant round-trips
    /// perfectly against itself. Only a known-answer test does — hence the
    /// literals spelled out here rather than referencing the constants. If this
    /// test fails, the fix is to restore the constant, **never** to update the
    /// literal.
    #[test]
    fn the_kek_context_strings_are_frozen() {
        assert_eq!(NOSTR_NSEC_CONTEXT, "fauna nostr key encryption 2026-03-19");
        assert_eq!(
            BUNKER_SIGNER_CONTEXT,
            "fauna nostr bunker signer key encryption 2026-07-20"
        );
        assert_eq!(
            ACTIVITYPUB_RSA_CONTEXT,
            "fauna activitypub key encryption 2026-03-20"
        );
        assert_eq!(
            ATPROTO_AUTHORING_CONTEXT,
            "fauna atproto authoring key encryption 2026-07-23"
        );
        assert_eq!(
            OAUTH_ISSUER_CONTEXT,
            "fauna oauth issuer key encryption 2026-09-08"
        );
        assert_eq!(
            ROOM_READ_CONTEXT,
            "fauna nest room read key encryption 2026-09-09"
        );
        assert_eq!(
            OAUTH_SESSION_CONTEXT,
            "fauna oauth session secret encryption 2026-09-09"
        );
        assert_eq!(
            MEDIA_TICKET_CONTEXT,
            "fauna media ticket secret encryption 2026-10-02"
        );
        assert_eq!(
            FIRST_PARTY_BRIDGE_CONTEXT,
            "fauna first-party bridge key encryption 2026-10-03"
        );
        assert_eq!(
            MAIL_DKIM_CONTEXT,
            "fauna mail dkim key encryption 2026-10-03"
        );
    }

    /// Every context string must be distinct — two satellites sharing a derived
    /// key would let a ciphertext from one open as the other (rule #3).
    ///
    /// ActivityPub's two tables deliberately share one context (one key class,
    /// two homes), so the assertion is over the distinct *contexts* used.
    #[test]
    fn every_satellite_context_is_domain_separated() {
        let mut contexts: Vec<&str> = SATELLITES.iter().map(|s| s.context).collect();
        contexts.sort_unstable();
        contexts.dedup();
        assert_eq!(
            contexts.len(),
            10,
            "expected exactly ten distinct KEK contexts \
             (nsec, bunker, activitypub, authoring, oauth issuer, room read, oauth session, \
             media ticket, first-party bridge, mail dkim)"
        );
        // Proven functionally rather than by comparing key bytes: a ciphertext
        // sealed under one context must not open under any other. That is the
        // property rule #3 actually buys, and it survives a future change to how
        // the key is represented.
        let seed = [7u8; 32];
        for a in &contexts {
            let sealed =
                fauna_core::crypto::encrypt_backup_chunk(&derive(a, &seed), b"secret").unwrap();
            for b in &contexts {
                let opened = fauna_core::crypto::decrypt_backup_chunk(&derive(b, &seed), &sealed);
                if a == b {
                    assert_eq!(opened.unwrap(), b"secret");
                } else {
                    assert!(opened.is_err(), "context {b} opened {a}'s ciphertext");
                }
            }
        }
    }

    fn seed(b: u8) -> [u8; 32] {
        [b; 32]
    }

    /// The core property: after a re-key, every secret opens under the successor
    /// and none opens under the predecessor.
    #[test]
    fn a_rekey_moves_every_satellite_to_the_successor_seed() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE nostr_accounts (actor_id TEXT PRIMARY KEY, encrypted_privkey BLOB);
             CREATE TABLE nostr_bunker_signers (actor_id TEXT PRIMARY KEY, encrypted_privkey BLOB NOT NULL);
             CREATE TABLE atproto_authoring_keys (actor_id BLOB PRIMARY KEY, k_secret_wrapped BLOB NOT NULL);",
        )
        .unwrap();

        let (old, new) = (seed(1), seed(2));
        let nsec = b"the deposited nsec".to_vec();
        let signer = b"the bunker signer".to_vec();
        let authoring = b"the authoring sub-key".to_vec();

        conn.execute(
            "INSERT INTO nostr_accounts VALUES ('a', ?1)",
            rusqlite::params![
                fauna_core::crypto::encrypt_backup_chunk(&derive(NOSTR_NSEC_CONTEXT, &old), &nsec)
                    .unwrap()
            ],
        )
        .unwrap();
        // A remote-signing account deposits no key: the NULL must survive.
        conn.execute("INSERT INTO nostr_accounts VALUES ('b', NULL)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO nostr_bunker_signers VALUES ('a', ?1)",
            rusqlite::params![
                fauna_core::crypto::encrypt_backup_chunk(
                    &derive(BUNKER_SIGNER_CONTEXT, &old),
                    &signer
                )
                .unwrap()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO atproto_authoring_keys VALUES (x'aa', ?1)",
            rusqlite::params![
                fauna_core::crypto::encrypt_backup_chunk(
                    &derive(ATPROTO_AUTHORING_CONTEXT, &old),
                    &authoring
                )
                .unwrap()
            ],
        )
        .unwrap();

        // `ap_accounts` / `ap_instance_actor` are absent here on purpose: this
        // is the store-safe flavor's shape, and the walk must not fail on it.
        let n = reencrypt_satellites(&conn, &old, &new).unwrap();
        assert_eq!(n, 3, "three non-NULL ciphertexts re-keyed");

        let opens = |table: &str, col: &str, ctx: &str, s: &[u8; 32]| -> Option<Vec<u8>> {
            let ct: Option<Vec<u8>> = conn
                .query_row(&format!("SELECT {col} FROM {table} LIMIT 1"), [], |r| {
                    r.get(0)
                })
                .unwrap();
            fauna_core::crypto::decrypt_backup_chunk(&derive(ctx, s), &ct?).ok()
        };

        assert_eq!(
            opens(
                "nostr_accounts",
                "encrypted_privkey",
                NOSTR_NSEC_CONTEXT,
                &new
            ),
            Some(nsec)
        );
        assert_eq!(
            opens(
                "nostr_accounts",
                "encrypted_privkey",
                NOSTR_NSEC_CONTEXT,
                &old
            ),
            None,
            "the superseded seed must no longer open it — that IS the eviction"
        );
        assert_eq!(
            opens(
                "nostr_bunker_signers",
                "encrypted_privkey",
                BUNKER_SIGNER_CONTEXT,
                &new
            ),
            Some(signer)
        );
        assert_eq!(
            opens(
                "atproto_authoring_keys",
                "k_secret_wrapped",
                ATPROTO_AUTHORING_CONTEXT,
                &new
            ),
            Some(authoring),
            "the ATProto authoring sub-key is a member of this family — the ceremony's \
             original three-way enumeration would have stranded it"
        );

        let null_survived: Option<Vec<u8>> = conn
            .query_row(
                "SELECT encrypted_privkey FROM nostr_accounts WHERE actor_id = 'b'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            null_survived.is_none(),
            "a NULL column is not a key to re-key"
        );
    }

    /// A ciphertext that does not open under the predecessor must abort the
    /// whole walk — writing a second unopenable layer over it would turn a
    /// recoverable inconsistency into permanent loss.
    #[test]
    fn a_ciphertext_that_does_not_open_aborts_the_walk() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE nostr_accounts (actor_id TEXT PRIMARY KEY, encrypted_privkey BLOB);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nostr_accounts VALUES ('a', ?1)",
            rusqlite::params![b"not a valid sealed chunk".to_vec()],
        )
        .unwrap();
        assert!(reencrypt_satellites(&conn, &seed(1), &seed(2)).is_err());
    }

    /// The store-safe flavor has none of these tables. The walk must be a
    /// no-op, not an error — otherwise rotation would be impossible on exactly
    /// the build that ships to Windows.
    #[test]
    fn a_database_with_no_satellite_tables_rekeys_nothing() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(reencrypt_satellites(&conn, &seed(1), &seed(2)).unwrap(), 0);
    }
}
