//! The nest's OAuth issuer signing key **set** — custody, rotation and the
//! JWKS read (TP5 part 1).
//!
//! `docs/goal/architecture/key-material-hierarchy.md` § Audience: deployment
//! infrastructure → *Issuer signing key* rules the custody: a fresh random
//! ES256 keypair minted nest-side, its private half sealed under this family's
//! own dated context string ([`crate::nest_kek::OAUTH_ISSUER_CONTEXT`]), the
//! public half served at `/oauth/jwks` as a key **set** with `kid`s, and
//! rotation that "adds a key and retires the outgoing one after the
//! access-token horizon".
//!
//! # Why a set and not a key
//!
//! A token carries the `kid` of the key that signed it and stays valid for its
//! own lifetime. If rotation replaced the key, every token minted in the
//! seconds before it would fail verification against a JWKS that no longer
//! carried its `kid` — a self-inflicted outage on an operation the admin was
//! told is safe. So a rotation *adds*: the new key becomes the signer, the
//! outgoing key stops signing but keeps being **served**, and it is dropped
//! only once nothing it signed can still be live.
//!
//! # Why no cargo feature
//!
//! `docs/goal/behavior/authorization-server.md` § The issuer rules that the
//! authorization server "is up whenever the nest is up", and § Custody records
//! that the move to nest custody exists precisely so "the issuer no longer
//! depends on an optional bridge being enrolled and approved". This module and
//! everything it reaches therefore compile in every nest flavor — including the
//! Windows service exe, which builds no bridge features at all.
//!
//! # When it mints — at boot, never on a read
//!
//! [`mint_if_absent`] seats the active signer, and the boot step
//! ([`crate::nest_kek::mint_at_boot`]) is its only caller: it runs before a
//! serving generation answers anything. [`active_signer`] and
//! [`serve_key_set`] only look the set up, and [`serve_key_set`] takes no seed
//! at all — the public halves rest in the clear, so a read holding no seed
//! cannot seal anything. The two rotation arms, which mint on purpose, read the
//! seed out of `nest_keypair` inside their own transaction. So nothing seals a
//! key under a serving generation's copy of the seed, which inside a
//! deployment-seed rotation's hand-off window is the retired one
//! (`key-material-hierarchy.md` § Audience: deployment infrastructure →
//! *Room-read keypair* → *When it mints*).

use anyhow::{Context, Result};
use fauna_provisioning::oauth_issuer::{
    MintedIssuerKey, issuer_key_retirement_horizon_secs, mint_issuer_key,
};
use rusqlite::{Connection, OptionalExtension as _};

use crate::nest_kek::{
    NotProvisioned, OAUTH_ISSUER_CONTEXT, require_deployment_seed, unwrap_32, wrap_32,
};

/// What [`NotProvisioned`] names for this plane.
const ISSUER_KEY: &str = "OAuth issuer signing key";

/// One public half, as `/oauth/jwks` serves it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuerPublicKey {
    pub kid: String,
    pub x: String,
    pub y: String,
    /// NULL for the active signer; the retirement instant otherwise. Served
    /// either way — a retired key still verifies tokens it signed.
    pub retired_at: Option<i64>,
}

/// The active signer: the `kid` and the unsealed 32-byte scalar.
pub struct IssuerSigner {
    pub kid: String,
    pub secret_scalar: [u8; 32],
}

/// The active signing key — a lookup, never a mint.
///
/// No active key is [`NotProvisioned`]. An active key that does not open under
/// `deployment_seed` is an error too, so a caller holding a stale seed gets a
/// refusal, never a scalar.
pub fn active_signer(conn: &Connection, deployment_seed: &[u8; 32]) -> Result<IssuerSigner> {
    read_active(conn, deployment_seed)?.ok_or_else(|| NotProvisioned(ISSUER_KEY).into())
}

/// Seat an active signer if the set has none, and return the active `kid`.
/// Only the boot step ([`crate::nest_kek::mint_at_boot`]) calls it.
///
/// First-write-wins: the read and the insert run on the caller's connection,
/// under its one guard, so two boot steps cannot both find the set empty, and
/// the key is read back so the `kid` returned is one the database kept. An
/// active key that does not open under `deployment_seed` refuses rather than
/// minting a second signer beside it.
pub(crate) fn mint_if_absent(conn: &Connection, deployment_seed: &[u8; 32]) -> Result<String> {
    if let Some(signer) = read_active(conn, deployment_seed)? {
        return Ok(signer.kid);
    }
    let minted = mint_issuer_key().map_err(|e| anyhow::anyhow!("mint issuer key: {e}"))?;
    insert(conn, deployment_seed, &minted, now_secs())?;
    Ok(read_active(conn, deployment_seed)?
        .context("issuer key vanished between mint and read-back — refusing to sign unrecorded")?
        .kid)
}

/// Every public half the JWKS should carry — active first, then retired.
///
/// Ordered so a client that reads only the first entry gets the current signer.
/// A row whose stored coordinates are missing is skipped rather than fatal: one
/// unreadable key must not take the whole key set — and with it every live
/// token — off the air.
pub fn public_key_set(conn: &Connection) -> Result<Vec<IssuerPublicKey>> {
    let mut stmt = conn
        .prepare(
            "SELECT kid, x, y, retired_at FROM oauth_issuer_keys \
             ORDER BY retired_at IS NOT NULL, created_at DESC",
        )
        .context("prepare issuer JWKS read")?;
    let keys = stmt
        .query_map([], |r| {
            Ok(IssuerPublicKey {
                kid: r.get(0)?,
                x: r.get(1)?,
                y: r.get(2)?,
                retired_at: r.get(3)?,
            })
        })?
        .filter_map(|row| match row {
            Ok(key) => Some(key),
            Err(e) => {
                tracing::warn!(error = %e, "oauth-issuer: skipping unreadable JWKS row");
                None
            }
        })
        .collect();
    Ok(keys)
}

/// The key set as a reader should see it, with past-horizon retired keys
/// already gone — a lookup, never a mint.
///
/// No active key is [`NotProvisioned`] rather than an empty set: the JWKS, the
/// admin status read, `/oauth/revoke` and the PDS bridge's feed all read here,
/// and a client caching "this issuer has no keys" is what an empty answer would
/// buy. It takes no seed because it needs none — the public halves rest in the
/// clear — and a read holding no seed cannot seal a key under a stale one.
///
/// The **one** place the horizon is applied, and it is applied lazily on the
/// read rather than by a timer — the shape
/// `docs/goal/behavior/authorization-server.md` § As built ratified for the
/// bridge-held set ("lazily against the injected clock, no timers"), which
/// carries to nest custody unchanged. A timer would need a scheduler, would
/// fire on a nest nobody is asking, and would still have to be re-derived on
/// read for a nest that was asleep when it should have fired.
///
/// `now` is passed in rather than read here so a test can place the clock; the
/// horizon is not a parameter, because unlike the clock there is exactly one
/// right answer and [`fauna_provisioning::oauth_issuer`] owns it.
pub fn serve_key_set(conn: &Connection, now: i64) -> Result<Vec<IssuerPublicKey>> {
    let has_signer: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM oauth_issuer_keys WHERE retired_at IS NULL)",
            [],
            |r| r.get(0),
        )
        .context("look for the active issuer key")?;
    if !has_signer {
        return Err(NotProvisioned(ISSUER_KEY).into());
    }
    let horizon = i64::try_from(issuer_key_retirement_horizon_secs())
        .context("the issuer key retirement horizon does not fit an i64")?;
    sweep_retired(conn, horizon, now)?;
    public_key_set(conn)
}

/// Rotate: mint a new signer and retire the outgoing key(s).
///
/// Returns the new `kid`. Retired keys stay in the set — [`sweep_retired`] is
/// what eventually drops them, once the horizon has passed.
///
/// One transaction, because a mint that landed without the retirement would
/// leave two un-retired keys and no defined signer.
///
/// The seed is read out of `nest_keypair` inside that transaction, never taken
/// from the caller. An admin's rotation is answered by a serving generation,
/// and inside a deployment-seed rotation's hand-off window that generation's
/// copy of the seed is the retired one: a key sealed under it would never open
/// again, and the satellite walk would refuse it on every later rotation —
/// whether or not the set held a key before. The transaction holds the
/// database guard the rotation holds, so the seed read here is the one the
/// database holds when the key is written.
pub fn rotate(conn: &mut Connection) -> Result<String> {
    let minted = mint_issuer_key().map_err(|e| anyhow::anyhow!("mint issuer key: {e}"))?;
    let now = now_secs();
    let tx = conn.transaction().context("open issuer rotation")?;
    let deployment_seed = require_deployment_seed(&tx)?;
    tx.execute(
        "UPDATE oauth_issuer_keys SET retired_at = ?1 WHERE retired_at IS NULL",
        rusqlite::params![now],
    )
    .context("retire the outgoing issuer key")?;
    insert(&tx, &deployment_seed, &minted, now)?;
    tx.commit().context("commit issuer rotation")?;
    Ok(minted.kid)
}

/// What a forced rotation did: the new signer, and every key it removed.
pub struct ForcedRotation {
    /// The `kid` that now signs — the only key the set carries afterwards.
    pub kid: String,
    /// Every `kid` dropped: the outgoing signer and any retired key that was
    /// still inside its horizon. Empty only on a store nothing had minted in.
    pub dropped_kids: Vec<String>,
}

/// Force-rotate: mint a new signer and **drop every other key**, horizon
/// skipped — the compromise response.
///
/// `docs/goal/behavior/authorization-server.md` § The issuer → *Two rotation
/// arms*: [`rotate`] answers the availability question (an honest token minted
/// a second before the rotation must still verify, so the outgoing key stays
/// served for the horizon), and that is the wrong answer to the compromise
/// question — a thief holding the leaked scalar chooses `exp` themselves, so
/// the token lifetime bounds nothing they mint, and the only thing that does
/// is the leaked `kid` leaving the served set. This door removes it on the
/// spot: the very next [`serve_key_set`] carries the new signer and nothing
/// else, with no sweep involved.
///
/// Every key goes, not a named one — a leak is "the store was read at time
/// T", which takes every key that existed at T, so the admin is never asked
/// to guess which one the thief holds. The cost is exactly the horizon's
/// purpose inverted: every honest token signed by a dropped key stops
/// verifying now, and the app control states that before dispatch.
///
/// One transaction, for [`rotate`]'s reason and one more: a delete that landed
/// without the mint would leave the nest with no key at all, so every read
/// would answer [`NotProvisioned`] and nothing could be signed until the next
/// boot seated a key nobody asked for — while the admin's reply never arrived.
/// The mint lands first inside the transaction so the delete can name
/// "everything but the new kid" rather than "everything", which is what makes a
/// fresh store (nothing to drop) and a populated one the same code path. The
/// seed is read inside the transaction, for [`rotate`]'s reason.
pub fn force_rotate(conn: &mut Connection) -> Result<ForcedRotation> {
    let minted = mint_issuer_key().map_err(|e| anyhow::anyhow!("mint issuer key: {e}"))?;
    let now = now_secs();
    let tx = conn.transaction().context("open forced issuer rotation")?;
    let deployment_seed = require_deployment_seed(&tx)?;
    let dropped_kids: Vec<String> = tx
        .prepare("SELECT kid FROM oauth_issuer_keys ORDER BY created_at DESC")
        .context("prepare the pre-rotation kid read")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("read the kids a forced rotation drops")?;
    insert(&tx, &deployment_seed, &minted, now)?;
    tx.execute(
        "DELETE FROM oauth_issuer_keys WHERE kid <> ?1",
        rusqlite::params![minted.kid],
    )
    .context("drop every key but the new signer")?;
    tx.commit().context("commit forced issuer rotation")?;
    Ok(ForcedRotation {
        kid: minted.kid,
        dropped_kids,
    })
}

/// Drop retired keys whose horizon has passed.
///
/// `horizon_secs` is the access-token lifetime plus the JWKS-cache pickup
/// grace: a key retired longer ago than that can have signed nothing still
/// live. It is a PARAMETER rather than a constant here because the number is
/// owned elsewhere — a second copy of it in this module is exactly the drift
/// `authorization-server.md` § The issuer leaves to one owner. Callers should
/// reach [`serve_key_set`], which supplies it from that owner; the parameter
/// exists so a test can place the horizon as well as the clock.
///
/// Never drops the active key, whatever the horizon: `retired_at IS NOT NULL`
/// is the guard, and a nest with no key at all could not serve its own JWKS.
pub fn sweep_retired(conn: &Connection, horizon_secs: i64, now: i64) -> Result<usize> {
    let dropped = conn
        .execute(
            "DELETE FROM oauth_issuer_keys \
             WHERE retired_at IS NOT NULL AND retired_at <= ?1",
            rusqlite::params![now - horizon_secs],
        )
        .context("sweep retired issuer keys")?;
    Ok(dropped)
}

fn read_active(conn: &Connection, deployment_seed: &[u8; 32]) -> Result<Option<IssuerSigner>> {
    let row: Option<(String, Vec<u8>)> = conn
        .query_row(
            "SELECT kid, secret_wrapped FROM oauth_issuer_keys \
             WHERE retired_at IS NULL ORDER BY created_at DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .context("read the active issuer key")?;
    let Some((kid, wrapped)) = row else {
        return Ok(None);
    };
    let secret_scalar = unwrap_32(OAUTH_ISSUER_CONTEXT, deployment_seed, &wrapped)
        .with_context(|| format!("unseal issuer key {kid}"))?;
    Ok(Some(IssuerSigner { kid, secret_scalar }))
}

fn insert(
    conn: &Connection,
    deployment_seed: &[u8; 32],
    minted: &MintedIssuerKey,
    now: i64,
) -> Result<()> {
    let wrapped = wrap_32(OAUTH_ISSUER_CONTEXT, deployment_seed, &minted.secret_scalar)
        .context("seal the minted issuer key")?;
    conn.execute(
        "INSERT OR IGNORE INTO oauth_issuer_keys (kid, secret_wrapped, x, y, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![minted.kid, wrapped, minted.x, minted.y, now],
    )
    .context("insert the minted issuer key")?;
    Ok(())
}

fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE oauth_issuer_keys (
                 kid            TEXT PRIMARY KEY,
                 secret_wrapped BLOB NOT NULL,
                 x              TEXT NOT NULL,
                 y              TEXT NOT NULL,
                 created_at     INTEGER NOT NULL,
                 retired_at     INTEGER
             );
             CREATE TABLE nest_keypair (
                 id          INTEGER PRIMARY KEY CHECK (id = 1),
                 secret_key  BLOB NOT NULL,
                 public_key  BLOB NOT NULL,
                 created_at  INTEGER NOT NULL
             );",
        )
        .unwrap();
        conn
    }

    fn seed(b: u8) -> [u8; 32] {
        [b; 32]
    }

    /// Seat `seed` as the deployment keypair — what the two rotation arms seal
    /// under. Only the seed is read, so the public half is left empty.
    fn hold_deployment_seed(conn: &Connection, seed: &[u8; 32]) {
        conn.execute(
            "INSERT OR REPLACE INTO nest_keypair (id, secret_key, public_key, created_at)
             VALUES (1, ?1, x'', 0)",
            rusqlite::params![seed.as_slice()],
        )
        .unwrap();
    }

    /// A store the boot step has seated under `seed`, with `seed` as the
    /// deployment keypair — the shape every rotation test starts from.
    fn booted(seed: &[u8; 32]) -> Connection {
        let conn = db();
        hold_deployment_seed(&conn, seed);
        mint_if_absent(&conn, seed).unwrap();
        conn
    }

    fn rows(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM oauth_issuer_keys", [], |r| r.get(0))
            .expect("count")
    }

    /// The whole fix at its narrowest: an absent signer is an answer a read
    /// returns, never a gap it fills. Both read doors — the signer and the set
    /// the JWKS and the admin status read share — answer the named error, and
    /// neither hands a client an empty set it would cache as "this issuer has
    /// no keys".
    #[test]
    fn a_read_never_mints() {
        let conn = db();
        let signer = active_signer(&conn, &seed(1));
        let set = serve_key_set(&conn, now_secs());
        assert_eq!(rows(&conn), 0, "a read must never mint the issuer key");
        for err in [signer.err(), set.err()] {
            let err = err.expect("no row, so no key");
            assert!(
                err.is::<NotProvisioned>(),
                "an absent signer is the named NotProvisioned error, got: {err:#}"
            );
        }
    }

    #[test]
    fn the_boot_mint_seats_one_signer_and_a_second_mint_keeps_it() {
        // First-write-wins. A second mint that added a key would rotate the
        // issuer on every boot — every token signed by a key no longer active.
        let conn = db();
        let first = mint_if_absent(&conn, &seed(1)).unwrap();
        let second = mint_if_absent(&conn, &seed(1)).unwrap();
        assert_eq!(first, second);
        assert_eq!(active_signer(&conn, &seed(1)).unwrap().kid, first);
        assert_eq!(public_key_set(&conn).unwrap().len(), 1);
    }

    #[test]
    fn the_stored_secret_is_sealed_and_opens_to_the_advertised_kid() {
        // The two halves the JWKS and the signer each read must be halves of
        // ONE key: a row whose scalar does not match its published `x`/`y`
        // would mint tokens no client could verify, and every shape check
        // would pass.
        let conn = db();
        mint_if_absent(&conn, &seed(1)).unwrap();
        let signer = active_signer(&conn, &seed(1)).unwrap();

        let stored: Vec<u8> = conn
            .query_row("SELECT secret_wrapped FROM oauth_issuer_keys", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_ne!(
            stored.as_slice(),
            signer.secret_scalar.as_slice(),
            "the scalar must not rest in cleartext beside the rest of the DB"
        );

        let key = p256::ecdsa::SigningKey::from_bytes(&signer.secret_scalar.into()).unwrap();
        let published = &public_key_set(&conn).unwrap()[0];
        assert_eq!(
            fauna_provisioning::oauth_issuer::rfc7638_p256_thumbprint(key.verifying_key()),
            published.kid
        );
        let (x, y) =
            fauna_provisioning::oauth_issuer::public_coordinates(key.verifying_key()).unwrap();
        assert_eq!(
            (x.as_str(), y.as_str()),
            (published.x.as_str(), published.y.as_str())
        );
    }

    #[test]
    fn a_wrong_deployment_seed_fails_loudly_rather_than_minting_a_second_key() {
        // The dangerous failure: a mint under the wrong seed that silently
        // adds a NEW key, orphaning every token signed by the old one while
        // looking perfectly healthy. Both the read and the mint must refuse.
        let conn = db();
        mint_if_absent(&conn, &seed(1)).unwrap();
        assert!(active_signer(&conn, &seed(2)).is_err());
        assert!(mint_if_absent(&conn, &seed(2)).is_err());
        assert_eq!(public_key_set(&conn).unwrap().len(), 1);
    }

    #[test]
    fn rotation_adds_a_key_and_keeps_serving_the_outgoing_one() {
        // The whole reason this is a set: a token minted a second before the
        // rotation must still verify.
        let mut conn = booted(&seed(1));
        let before = active_signer(&conn, &seed(1)).unwrap();
        let new_kid = rotate(&mut conn).unwrap();

        assert_ne!(new_kid, before.kid);
        let set = public_key_set(&conn).unwrap();
        assert_eq!(set.len(), 2);
        assert_eq!(set[0].kid, new_kid, "the active signer is served first");
        assert!(set[0].retired_at.is_none());
        let outgoing = set.iter().find(|k| k.kid == before.kid).unwrap();
        assert!(
            outgoing.retired_at.is_some(),
            "the outgoing key is retired, not deleted"
        );
        assert_eq!(active_signer(&conn, &seed(1)).unwrap().kid, new_kid);
    }

    #[test]
    fn the_sweep_drops_a_key_past_the_horizon_and_never_the_active_one() {
        // A sweep that could take the active key would leave the nest with no
        // signer and an empty JWKS — the horizon is about retired keys only.
        let mut conn = booted(&seed(1));
        let new_kid = rotate(&mut conn).unwrap();
        let now = now_secs();

        assert_eq!(
            sweep_retired(&conn, 900, now).unwrap(),
            0,
            "a key retired just now is still inside the horizon"
        );
        assert_eq!(public_key_set(&conn).unwrap().len(), 2);

        assert_eq!(sweep_retired(&conn, 900, now + 901).unwrap(), 1);
        let set = public_key_set(&conn).unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].kid, new_kid);

        // Even an absurd horizon leaves the signer: it is guarded by
        // `retired_at IS NOT NULL`, not by arithmetic.
        assert_eq!(sweep_retired(&conn, 0, now + 10_000_000).unwrap(), 0);
        assert_eq!(public_key_set(&conn).unwrap().len(), 1);
    }

    #[test]
    fn the_served_set_applies_the_horizon_itself_so_no_caller_has_to() {
        // `sweep_retired` takes the horizon as a parameter, which is right —
        // but it means a reader that forgot to call it would serve a retired
        // key forever, and nothing else in the system would notice. This pins
        // the read door as the place the horizon is actually applied, on the
        // shared owner's number rather than a local literal.
        let mut conn = booted(&seed(1));
        let outgoing = active_signer(&conn, &seed(1)).unwrap().kid;
        let new_kid = rotate(&mut conn).unwrap();
        let now = now_secs();
        let horizon = i64::try_from(issuer_key_retirement_horizon_secs()).unwrap();

        let inside: Vec<_> = serve_key_set(&conn, now)
            .unwrap()
            .into_iter()
            .map(|k| k.kid)
            .collect();
        assert_eq!(
            inside,
            vec![new_kid.clone(), outgoing],
            "inside the horizon both keys are served, signer first"
        );

        let past: Vec<_> = serve_key_set(&conn, now + horizon + 1)
            .unwrap()
            .into_iter()
            .map(|k| k.kid)
            .collect();
        assert_eq!(
            past,
            vec![new_kid],
            "past the horizon only the signer remains"
        );
    }

    /// The whole reason `nest_kek::SATELLITES` carries an `oauth_issuer_keys`
    /// entry: a deployment-seed rotation must re-key this table with the rest
    /// of the family.
    ///
    /// Without the entry the walk skips the table, the rows keep their
    /// old-seed ciphertext, and the next read fails — the issuer key is
    /// **orphaned by a ceremony advertised as safe**, and every outstanding
    /// token with it. The failure is silent at rotation time and only shows up
    /// the next time anything asks the nest to sign, which is why this is
    /// pinned end-to-end (mint under one seed, walk, read under the other)
    /// rather than by asserting the constant appears in the list: a list
    /// membership test passes just as happily when the walk itself is broken.
    #[test]
    fn a_deployment_seed_rotation_carries_the_issuer_key_rather_than_orphaning_it() {
        let conn = db();
        mint_if_absent(&conn, &seed(1)).unwrap();
        let before = active_signer(&conn, &seed(1)).unwrap();

        let rewritten = crate::nest_kek::reencrypt_satellites(&conn, &seed(1), &seed(2)).unwrap();
        assert_eq!(
            rewritten, 1,
            "the issuer key must be one of the re-keyed rows"
        );

        let after = active_signer(&conn, &seed(2)).unwrap();
        assert_eq!(
            after.kid, before.kid,
            "the same key, re-sealed — not a new mint"
        );
        assert_eq!(after.secret_scalar, before.secret_scalar);
        assert_eq!(
            public_key_set(&conn).unwrap().len(),
            1,
            "a re-key adds no key to the set"
        );
        assert!(
            active_signer(&conn, &seed(1)).is_err(),
            "the superseded seed must no longer open it"
        );
    }

    /// A retired key is re-keyed too. It is still served, so it must still
    /// open: a walk that only carried the active row would strand exactly the
    /// keys whose tokens are still in flight.
    #[test]
    fn a_seed_rotation_carries_the_retired_keys_as_well() {
        let mut conn = booted(&seed(1));
        rotate(&mut conn).unwrap();

        let rewritten = crate::nest_kek::reencrypt_satellites(&conn, &seed(1), &seed(2)).unwrap();
        assert_eq!(
            rewritten, 2,
            "both the active and the retired key are re-keyed"
        );
        assert_eq!(public_key_set(&conn).unwrap().len(), 2);
        assert!(active_signer(&conn, &seed(2)).is_ok());
    }

    #[test]
    fn a_forced_rotation_drops_every_other_key_with_no_horizon_wait() {
        // The compromise response. A thief holding a leaked scalar chooses
        // `exp` themselves, so the only thing bounding a forged token is the
        // leaked `kid` leaving the served set — and the ordinary arm keeps it
        // there for the whole horizon. This pins the forced arm as the door
        // that removes it NOW: the set the very next read serves carries the
        // new signer and nothing else, at the same instant, no sweep involved.
        //
        // A key retired moments ago is still inside its horizon and still
        // served — a leak of the store takes it too, so it must go too. So the
        // fixture is one ordinary rotation: `leaked_retired` outgoing,
        // `now_active` signing, both inside the horizon.
        let mut conn = booted(&seed(1));
        let leaked_retired = active_signer(&conn, &seed(1)).unwrap().kid;
        let now_active = rotate(&mut conn).unwrap();
        assert_ne!(leaked_retired, now_active);
        let now = now_secs();
        assert_eq!(
            serve_key_set(&conn, now).unwrap().len(),
            2,
            "precondition: the ordinary arm left the outgoing key served"
        );

        let forced = force_rotate(&mut conn).unwrap();

        assert_ne!(forced.kid, now_active);
        assert_ne!(forced.kid, leaked_retired);
        let mut dropped = forced.dropped_kids.clone();
        dropped.sort();
        let mut expected = vec![now_active.clone(), leaked_retired.clone()];
        expected.sort();
        assert_eq!(
            dropped, expected,
            "every key that existed before the call is reported dropped"
        );

        let served: Vec<_> = serve_key_set(&conn, now)
            .unwrap()
            .into_iter()
            .map(|k| k.kid)
            .collect();
        assert_eq!(
            served,
            vec![forced.kid.clone()],
            "the very next read serves the new signer and NOTHING else — no horizon wait"
        );
        assert_eq!(
            active_signer(&conn, &seed(1)).unwrap().kid,
            forced.kid,
            "the new key signs"
        );
    }

    #[test]
    fn a_forced_rotation_on_a_fresh_nest_still_leaves_exactly_one_signer() {
        // The degenerate case — nothing to drop but the signer itself — must
        // still end with one un-retired key, never zero (a nest with no key
        // could not serve its own JWKS) and never two.
        let mut conn = booted(&seed(1));
        let before = active_signer(&conn, &seed(1)).unwrap().kid;
        let forced = force_rotate(&mut conn).unwrap();
        assert_eq!(forced.dropped_kids, vec![before]);
        let set = public_key_set(&conn).unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].kid, forced.kid);
        assert!(set[0].retired_at.is_none());
    }

    #[test]
    fn a_forced_rotation_on_an_empty_store_mints_and_drops_nothing() {
        // The boot step never seated a key (the boot step failed). The forced arm must not fail on
        // "nothing to drop" — the admin's intent is "a key nobody else holds
        // signs from now on", which an empty store satisfies by minting.
        let mut conn = db();
        hold_deployment_seed(&conn, &seed(1));
        let forced = force_rotate(&mut conn).unwrap();
        assert!(forced.dropped_kids.is_empty());
        assert_eq!(public_key_set(&conn).unwrap().len(), 1);
        assert_eq!(active_signer(&conn, &seed(1)).unwrap().kid, forced.kid);
    }

    #[test]
    fn a_rotation_leaves_exactly_one_unretired_key_however_often_it_runs() {
        // Two un-retired rows would make "the active signer" a matter of which
        // row the ORDER BY happened to reach first.
        let mut conn = booted(&seed(1));
        for _ in 0..3 {
            rotate(&mut conn).unwrap();
        }
        let set = public_key_set(&conn).unwrap();
        assert_eq!(set.len(), 4);
        assert_eq!(set.iter().filter(|k| k.retired_at.is_none()).count(), 1);
    }

    /// Both rotation arms seal under the seed the database holds and take none
    /// from their caller — inside a deployment-seed rotation's hand-off window a
    /// serving generation's copy is the retired one. Pinned here at the module;
    /// `oauth_issuer_handlers::tests::rotation_window` drives the same doors
    /// through a real ceremony.
    #[test]
    fn both_rotation_arms_seal_under_the_seed_the_database_holds() {
        let every_key_opens_under = |conn: &Connection, seed: &[u8; 32]| {
            let wrapped: Vec<Vec<u8>> = conn
                .prepare("SELECT secret_wrapped FROM oauth_issuer_keys")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            wrapped
                .iter()
                .all(|w| unwrap_32(OAUTH_ISSUER_CONTEXT, seed, w).is_ok())
        };

        let mut conn = booted(&seed(1));
        crate::nest_kek::reencrypt_satellites(&conn, &seed(1), &seed(2)).unwrap();
        hold_deployment_seed(&conn, &seed(2));

        rotate(&mut conn).unwrap();
        assert!(
            every_key_opens_under(&conn, &seed(2)),
            "the ordinary arm sealed its key under a seed the database does not hold"
        );

        force_rotate(&mut conn).unwrap();
        assert!(
            every_key_opens_under(&conn, &seed(2)),
            "the forced arm sealed its key under a seed the database does not hold"
        );
        assert!(active_signer(&conn, &seed(1)).is_err());
    }

    /// A nest with no deployment keypair has nothing to seal under, so both
    /// rotation arms refuse and write nothing.
    #[test]
    fn a_rotation_without_a_deployment_keypair_refuses() {
        let mut conn = db();
        assert!(rotate(&mut conn).is_err());
        assert!(force_rotate(&mut conn).is_err());
        assert_eq!(rows(&conn), 0);
    }
}
