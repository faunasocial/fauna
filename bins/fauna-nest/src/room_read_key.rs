//! The nest's **room-read keypair** — the wrap target that makes this nest a
//! readable member of the community rooms it homes.
//!
//! `docs/goal/architecture/key-material-hierarchy.md` § Audience: deployment
//! infrastructure → *Room-read keypair* rules it: one reception keypair per
//! nest **in the recipient-set scheme's own shape** — the same X-Wing wrap
//! format members use — of the service class the nest already holds beside
//! the deployment identity and the issuer signing key.
//!
//! # What it opens, and what it does not
//!
//! The room generation keys of the community rooms homed on this nest, each
//! wrapped to it by an owner or admin device at admission and on every
//! rotation, one recipient among the members'
//! (`docs/goal/behavior/conversation-rooms.md` § The three classes →
//! *Community*). **The nest reads; it never mints or rotates.** Key authority
//! stays with the room's owner and admins, so the nest's readable position is
//! a grant the room's members made — revocable by rotating the generation and
//! leaving this key out of the recipient set, which is their act, not this
//! module's.
//!
//! It gives no reach into any end-to-end room: no wrap is ever made to it
//! there, which is what makes "never presume a bridge or plugin is an MLS
//! group member" a theorem rather than a rule. And none into a room homed
//! elsewhere — a relay nest holds ciphertext only.
//!
//! # Why the shape is a satellite and not a seed derivation
//!
//! The goal doc says "minted from the deployment seed at first boot", which
//! reads either way; the resolution (and its reason) is recorded there and in
//! [`crate::nest_kek::ROOM_READ_CONTEXT`]. In short: this key's **public**
//! half rests in rows other members minted, so its identity must survive a
//! deployment-seed rotation. A seed-derived keypair would change identity on
//! rotation and silently make every community room's wrap to this nest
//! unopenable, with no ceremony able to rewrite rows on other people's
//! planes. A stable random ikm sealed under a seed-derived KEK has the
//! security property the doc actually claims — "DB-only exfiltration of the
//! wraps opens nothing without the deployment seed" — and `reencrypt_satellites`
//! re-keys the row on rotation while the reception public key stays put.
//!
//! The ikm rather than the expanded keypair is what rests, because X-Wing
//! keygen is deterministic from it: the member-side kind's own discipline
//! (`fauna_core::group_generation::GroupReceptionKeyRecord`), reused rather
//! than re-invented, down to the two frozen derivation contexts — so the nest
//! is a recipient of exactly the same kind as every other member, not a
//! parallel one.
//!
//! # When it mints — at boot, never on a read
//!
//! [`mint_if_absent`] is the only door that writes the row, and the boot step
//! [`crate::nest_kek::mint_at_boot`] is its only caller — a step `start_server`
//! alone runs, before the serving generation it builds answers anything (both
//! pinned tree-wide by that module's ratchet test). Every consumer —
//! `fauna.nest.info`, the birth ceremony's seat, the nest's own index open —
//! goes through [`room_read_key`] or [`public_key`], which only look the row
//! up and answer [`NotProvisioned`] when it is absent.
//!
//! The rule exists for the deployment-seed rotation's hand-off window
//! (`docs/goal/architecture/nest/box-recovery.md` § Deployment-seed rotation →
//! *The bounded hand-off window*). After the ceremony commits, the outgoing
//! serving generation still holds the retired seed in memory. A read that
//! minted there sealed the row under a seed the box no longer has: a row that
//! never opens again, and that `reencrypt_satellites` refuses on every later
//! rotation, so no further rotation could commit.
//!
//! The boot mint is immune to the same window for a structural reason, not a
//! timing one: it reads the seed out of `nest_keypair` on the connection it
//! inserts on, under the one database guard the rotation also holds for its
//! whole transaction. So it seals under whichever seed the database holds at
//! that instant, never under a copy some serving generation took earlier.

use anyhow::{Context, Result};
use fauna_core::group_generation::GroupReceptionKeyRecord;
use rusqlite::{Connection, OptionalExtension as _};

use crate::nest_kek::{NotProvisioned, ROOM_READ_CONTEXT, unwrap_32, wrap_32};

/// The nest's room-read keypair, unsealed.
///
/// Carried as the **member-side kind's own record**
/// ([`GroupReceptionKeyRecord`]) rather than an expanded keypair, so the nest
/// is literally a recipient of the same kind as every other member of a room
/// — same ikm discipline, same two frozen derivation contexts, same
/// `record.keypair()` call the wrap and unwrap paths already make. A
/// nest-specific parallel type would have compiled and only looked alike.
pub struct RoomReadKey {
    pub record: GroupReceptionKeyRecord,
    /// The reception **public** key, as it rests in the row and as members
    /// wrap to it.
    pub public_key: Vec<u8>,
}

/// The nest's room-read keypair — a lookup, never a mint.
///
/// An absent row is [`NotProvisioned`]. A row that does not open under
/// `deployment_seed` is an error too, so a caller holding a stale seed gets a
/// refusal, never a key.
pub fn room_read_key(conn: &Connection, deployment_seed: &[u8; 32]) -> Result<RoomReadKey> {
    read(conn, deployment_seed)?.ok_or_else(|| NotProvisioned("room-read keypair").into())
}

/// The reception **public** key alone — a lookup, never a mint.
///
/// What `fauna.nest.info` publishes so a wrapping device can seal a room's
/// generation key to this nest. Public by construction — it is a KEM public
/// key, and a member must have it before it can make this nest a member.
pub fn public_key(conn: &Connection, deployment_seed: &[u8; 32]) -> Result<Vec<u8>> {
    Ok(room_read_key(conn, deployment_seed)?.public_key)
}

/// Mint-if-absent under `deployment_seed`, first-write-wins. Only the boot step
/// ([`crate::nest_kek::mint_at_boot`]) calls it.
///
/// Two concurrent mints (two embedded serving generations starting at once, a
/// test fixture beside a boot) race harmlessly because the insert is
/// `INSERT OR IGNORE` on the single `id = 0` row and whichever row wins is
/// read back. So both return the same key rather than one publishing a public
/// half whose secret the database never kept — which on this plane would be
/// worse than on the issuer's: members would wrap to a key nothing can open.
pub(crate) fn mint_if_absent(conn: &Connection, deployment_seed: &[u8; 32]) -> Result<RoomReadKey> {
    if let Some(key) = read(conn, deployment_seed)? {
        return Ok(key);
    }
    let mut ikm = [0u8; 32];
    getrandom::fill(&mut ikm).context("mint room-read ikm")?;
    let public_key = public_of(&ikm)?;
    let wrapped = wrap_32(ROOM_READ_CONTEXT, deployment_seed, &ikm)
        .context("wrap the room-read ikm under the deployment seed")?;
    conn.execute(
        "INSERT OR IGNORE INTO nest_room_read_key (id, ikm_wrapped, public_key, created_at)
         VALUES (0, ?1, ?2, ?3)",
        rusqlite::params![wrapped, public_key, crate::db::now_epoch_secs()],
    )
    .context("insert the room-read keypair")?;
    read(conn, deployment_seed)?.context(
        "the room-read keypair vanished between mint and read-back — refusing to publish \
         a public half whose secret is unrecorded",
    )
}

fn read(conn: &Connection, deployment_seed: &[u8; 32]) -> Result<Option<RoomReadKey>> {
    let row: Option<(Vec<u8>, Vec<u8>)> = conn
        .query_row(
            "SELECT ikm_wrapped, public_key FROM nest_room_read_key WHERE id = 0",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .context("read the room-read keypair")?;
    let Some((wrapped, public_key)) = row else {
        return Ok(None);
    };
    let ikm = unwrap_32(ROOM_READ_CONTEXT, deployment_seed, &wrapped)
        .context("unwrap the room-read ikm — wrong deployment seed, or a corrupted row")?;
    // The stored public half is what members wrapped to. If re-deriving from
    // the ikm does not reproduce it, the row is internally inconsistent —
    // refuse rather than hand back a keypair that opens nothing anyone sent.
    if public_of(&ikm)? != public_key {
        anyhow::bail!(
            "the stored room-read public key does not match the one its ikm derives — \
             refusing to serve a reception key that cannot open what members wrapped to it"
        );
    }
    Ok(Some(RoomReadKey {
        record: record_of(&ikm),
        public_key,
    }))
}

/// The record an ikm names — the member-side kind, so the derivation contexts
/// are `GroupReceptionKeyRecord`'s own and cannot drift from the ones members
/// use. `minted_at_ms` is advisory on that type and the row carries the real
/// stamp, so `0` here is honest rather than a fabricated instant.
fn record_of(ikm: &[u8; 32]) -> GroupReceptionKeyRecord {
    GroupReceptionKeyRecord {
        ikm: fauna_core::secret::SecretByteBuf::from(ikm.to_vec()),
        minted_at_ms: 0,
    }
}

fn public_of(ikm: &[u8; 32]) -> Result<Vec<u8>> {
    Ok(record_of(ikm)
        .keypair()
        .map_err(|e| anyhow::anyhow!("derive the room-read keypair: {e}"))?
        .public
        .to_bytes()
        .to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE nest_room_read_key (
                 id           INTEGER PRIMARY KEY CHECK(id = 0),
                 ikm_wrapped  BLOB NOT NULL,
                 public_key   BLOB NOT NULL,
                 created_at   INTEGER NOT NULL
             );",
        )
        .unwrap();
        conn
    }

    fn rows(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM nest_room_read_key", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn the_key_is_minted_once_and_read_back_stably() {
        let conn = db();
        let seed = [7u8; 32];
        let first = mint_if_absent(&conn, &seed).unwrap();
        let again = mint_if_absent(&conn, &seed).unwrap();
        let read_back = room_read_key(&conn, &seed).unwrap();
        assert_eq!(
            first.public_key, again.public_key,
            "a second mint is first-write-wins — it returns the key the row already holds"
        );
        assert_eq!(first.public_key, read_back.public_key);
        assert!(!first.public_key.is_empty());
        assert_eq!(rows(&conn), 1);
    }

    /// The whole fix at its narrowest: an absent row
    /// is an answer a read returns, never a gap it fills.
    #[test]
    fn a_read_never_mints() {
        let conn = db();
        let seed = [7u8; 32];
        // `.err().expect` rather than `expect_err`: `RoomReadKey` carries the
        // unsealed keypair and deliberately implements no `Debug`.
        #[allow(clippy::err_expect)]
        let err = room_read_key(&conn, &seed)
            .err()
            .expect("no row, so no key");
        assert!(
            err.is::<NotProvisioned>(),
            "an absent row is the named NotProvisioned error, got: {err:#}"
        );
        assert!(public_key(&conn, &seed).is_err());
        assert_eq!(rows(&conn), 0, "a read must never mint the room-read row");
    }

    /// The property the whole satellite shape exists for: a deployment-seed
    /// rotation re-keys the row and the reception **public key stays put**,
    /// so every wrap other members already minted to this nest keeps opening.
    /// A seed-derived keypair would fail this test, which is why the goal
    /// doc's "minted from the deployment seed" resolves the way it does.
    #[test]
    fn a_seed_rotation_keeps_the_reception_public_key() {
        let conn = db();
        let old_seed = [1u8; 32];
        let new_seed = [2u8; 32];
        let before = mint_if_absent(&conn, &old_seed).unwrap().public_key;

        crate::nest_kek::reencrypt_satellites(&conn, &old_seed, &new_seed).unwrap();

        let after = room_read_key(&conn, &new_seed).unwrap().public_key;
        assert_eq!(
            before, after,
            "the reception public key must survive a deployment-seed rotation — it rests \
             in wraps this nest cannot rewrite"
        );
    }

    #[test]
    fn the_wrong_seed_refuses_rather_than_deriving_a_stranger() {
        let conn = db();
        mint_if_absent(&conn, &[1u8; 32]).unwrap();
        assert!(
            room_read_key(&conn, &[9u8; 32]).is_err(),
            "a wrong deployment seed must refuse the read"
        );
        assert!(
            mint_if_absent(&conn, &[9u8; 32]).is_err(),
            "a wrong deployment seed must refuse, never silently mint a second key"
        );
        assert_eq!(rows(&conn), 1);
    }

    /// The nest derives its reception keypair through the member-side kind's
    /// own contexts, so a room's wrap code path treats it as an ordinary
    /// recipient. Pinned because a nest-specific context would compile and
    /// silently make the nest un-wrappable-to by the shared builder.
    #[test]
    fn the_nest_derives_the_same_kind_of_reception_key_as_a_member() {
        let ikm = [3u8; 32];
        let member = GroupReceptionKeyRecord {
            ikm: fauna_core::secret::SecretByteBuf::from(ikm.to_vec()),
            minted_at_ms: 1_700_000_000_000,
        };
        assert_eq!(
            public_of(&ikm).unwrap(),
            member.keypair().unwrap().public.to_bytes().to_vec(),
            "the nest's room-read keypair is the member-side group-reception kind, \
             derived identically from the same ikm — and `minted_at_ms` is advisory, \
             so it does not enter the derivation"
        );
    }

    /// The deployment-seed rotation's hand-off window, driven causally rather
    /// than by timing: the ceremony has committed (the DB holds the successor)
    /// while the serving generation it has not yet torn down still holds the
    /// retired seed in memory — `box-recovery.md` § Deployment-seed rotation →
    /// *The bounded hand-off window*.
    mod rotation_window {
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

        /// A serving generation built from `seed` — what the teardown replaces.
        fn generation(db: &Arc<CacheDb>, seed: &[u8; 32]) -> crate::routes::AppState {
            let mut state = crate::routes::AppState::for_test(db.clone());
            state.nest_signing_key = Some(ed25519_dalek::SigningKey::from_bytes(seed));
            state
        }

        /// The boot step `start_server` runs — its room-read member's result.
        async fn boot(db: &Arc<CacheDb>) -> Option<Vec<u8>> {
            crate::nest_kek::mint_at_boot(db)
                .await
                .unwrap()
                .map(|minted| {
                    minted
                        .room_read_public_key
                        .expect("the room-read member seats")
                })
        }

        async fn room_read_rows(db: &Arc<CacheDb>) -> i64 {
            let db = db.clone();
            tokio::task::spawn_blocking(move || super::rows(&db.conn_blocking()))
                .await
                .unwrap()
        }

        async fn opens_under(db: &Arc<CacheDb>, seed: &[u8; 32]) -> bool {
            let (db, seed) = (db.clone(), *seed);
            tokio::task::spawn_blocking(move || {
                super::room_read_key(&db.conn_blocking(), &seed).is_ok()
            })
            .await
            .unwrap()
        }

        /// The review's shape: no row exists when the ceremony runs, so the
        /// satellite walk re-keys nothing, and an anonymous `fauna.nest.info`
        /// answered by the outgoing generation lands inside the window. It must
        /// mint nothing; the successor generation's boot mint then seals under
        /// the successor seed, and the next rotation commits.
        #[tokio::test]
        async fn a_read_in_the_rotation_window_mints_nothing_and_the_next_rotation_commits() {
            let (a, b, c) = (seed(0xa1), seed(0xb2), seed(0xc3));
            let db = nest_with_deployment_seed(&a).await;
            let outgoing = generation(&db, &a);
            assert_eq!(room_read_rows(&db).await, 0, "precondition: no row yet");

            db.rotate_deployment_seed(&a, &b).await.unwrap().unwrap();

            let info = crate::discovery_core::nest_info_core(&outgoing).await;
            assert_eq!(
                room_read_rows(&db).await,
                0,
                "a read inside the rotation window minted the room-read row under the \
                 retired deployment seed"
            );
            assert!(
                info.room_read_pubkey.is_none(),
                "the outgoing generation has no room-read key to publish"
            );

            // The teardown re-enters `start_server`, whose boot mint runs first.
            let minted = boot(&db)
                .await
                .expect("the rotated nest still holds a deployment keypair");
            assert!(
                opens_under(&db, &b).await && !opens_under(&db, &a).await,
                "the boot mint seals under the seed the database holds, not a retired copy"
            );
            let successor = generation(&db, &b);
            assert_eq!(
                crate::discovery_core::nest_info_core(&successor)
                    .await
                    .room_read_pubkey,
                Some(minted.clone())
            );

            let next = db.rotate_deployment_seed(&b, &c).await.unwrap();
            let next = next.expect("the next rotation commits — nothing wedges the satellite walk");
            assert!(
                next.satellites_rekeyed >= 1,
                "the room-read row rides the rotation"
            );
            assert_eq!(
                crate::discovery_core::nest_info_core(&generation(&db, &c))
                    .await
                    .room_read_pubkey,
                Some(minted),
                "and its reception public key stays put"
            );
        }

        /// The ordinary shape once the fix is in: the row was minted at boot,
        /// the ceremony re-keys it, and a read in the window opens nothing and
        /// mints nothing — the benign path, which is now the only one.
        #[tokio::test]
        async fn a_boot_minted_key_rides_a_rotation_with_a_read_in_its_window() {
            let (a, b, c) = (seed(0xa1), seed(0xb2), seed(0xc3));
            let db = nest_with_deployment_seed(&a).await;
            let minted = boot(&db).await.unwrap();
            let outgoing = generation(&db, &a);

            db.rotate_deployment_seed(&a, &b).await.unwrap().unwrap();

            let info = crate::discovery_core::nest_info_core(&outgoing).await;
            assert!(
                info.room_read_pubkey.is_none(),
                "the retired seed no longer opens the re-keyed row"
            );
            assert_eq!(
                room_read_rows(&db).await,
                1,
                "and the read minted no second row"
            );

            // The successor generation's boot mint finds the row and keeps it.
            assert_eq!(boot(&db).await, Some(minted.clone()));
            assert_eq!(
                crate::discovery_core::nest_info_core(&generation(&db, &b))
                    .await
                    .room_read_pubkey,
                Some(minted)
            );
            db.rotate_deployment_seed(&b, &c)
                .await
                .unwrap()
                .expect("the next rotation commits");
        }
    }
}
