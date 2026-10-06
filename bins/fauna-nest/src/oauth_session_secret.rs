//! The authorization server's **second signer** — the HS256 secret this nest's
//! OAuth refresh tokens are MACed under.
//!
//! `docs/goal/architecture/key-material-hierarchy.md` § Audience: deployment
//! infrastructure → *OAuth refresh-token signing secret* owns the custody
//! entry; `docs/goal/behavior/authorization-server.md` § The issuer →
//! *Two HS256 secrets, not one* owns the re-decision that this key exists at
//! all.
//!
//! # Why the nest mints its own rather than reading the bridge's
//!
//! § As built recorded "one HS256 secret now signs two planes' refresh
//! tokens". That was a consequence of one process minting for both planes, not
//! a requirement — and nest custody removes the premise: the bridge's secret is
//! a nest-held blob **sealed to the bridge**, which this nest cannot read, and
//! it also signs the app-credential plane's `refreshSession` tokens, which stay
//! on the bridge. Reading it here would mean unsealing a blob addressed to
//! another principal; sharing a fresh one would put an app-plane signer inside
//! the nest for no reason. So: two secrets, one per plane's signer.
//!
//! ⚠ The **plane claim on the token stays** even though separate secrets
//! already make a cross-plane token fail its MAC. It covers a case the split
//! does not: an OAuth refresh token minted by the *bridge* under the shared
//! secret is still presentable at `com.atproto.server.refreshSession`, which
//! verifies under that same secret.
//!
//! # Why this is one row and the issuer key is a set
//!
//! The issuer key's public half is verified by strangers, so a rotation must
//! keep the outgoing generation servable for as long as tokens signed by it
//! live. This secret never leaves the box: it is presented back to this nest's
//! own `/oauth/token` and `/oauth/revoke` and to nothing else, so no generation
//! but the current one has a verifier to satisfy. A second row would only be a
//! second way to sign.
//!
//! # When it mints — at boot, never on a read
//!
//! [`mint_if_absent`] seats the row, and the boot step
//! ([`crate::nest_kek::mint_at_boot`]) is its only caller: it runs before a
//! serving generation answers anything. [`session_secret`] only looks the row
//! up. [`force_rotate`], the one door that re-mints on purpose, reads the seed
//! out of `nest_keypair` inside its own transaction. So nothing seals this row
//! under a serving generation's copy of the seed, which inside a
//! deployment-seed rotation's hand-off window is the retired one
//! (`key-material-hierarchy.md` § Audience: deployment infrastructure →
//! *Room-read keypair* → *When it mints*).

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension as _};

use crate::nest_kek::{
    NotProvisioned, OAUTH_SESSION_CONTEXT, require_deployment_seed, unwrap_32, wrap_32,
};

/// The OAuth refresh-token signing secret — a lookup, never a mint.
///
/// An absent row is [`NotProvisioned`]. A row that does not open under
/// `deployment_seed` is an error too, so a caller holding a stale seed gets a
/// refusal, never a secret.
pub fn session_secret(conn: &Connection, deployment_seed: &[u8; 32]) -> Result<[u8; 32]> {
    read(conn, deployment_seed)?.ok_or_else(|| NotProvisioned("OAuth session secret").into())
}

/// Mint the secret if the row is absent, first-write-wins. Only the boot step
/// ([`crate::nest_kek::mint_at_boot`]) calls it.
///
/// Two concurrent mints race harmlessly because the insert is `INSERT OR
/// IGNORE` on the single `id = 0` row and whichever row wins is read back, so
/// no caller signs under a secret the database never kept. A row that does not
/// open under `deployment_seed` refuses rather than being minted over.
pub(crate) fn mint_if_absent(conn: &Connection, deployment_seed: &[u8; 32]) -> Result<()> {
    if read(conn, deployment_seed)?.is_some() {
        return Ok(());
    }
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).context("mint the OAuth session secret")?;
    let wrapped = wrap_32(OAUTH_SESSION_CONTEXT, deployment_seed, &secret)
        .context("wrap the OAuth session secret under the deployment seed")?;
    conn.execute(
        "INSERT OR IGNORE INTO oauth_session_secret (id, secret_wrapped, created_at)
         VALUES (0, ?1, ?2)",
        rusqlite::params![wrapped, crate::db::now_epoch_secs()],
    )
    .context("insert the OAuth session secret")?;
    read(conn, deployment_seed)?.context(
        "the OAuth session secret vanished between mint and read-back — refusing to sign \
         refresh tokens under a secret the database never kept",
    )?;
    Ok(())
}

/// What [`force_rotate`] did: the instant the new secret was minted, and when
/// the secret it replaced had been minted — `None` on a store that held none
/// yet, which the door treats as the same rotation (a fresh mint) rather than a
/// distinct outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForcedSessionRotation {
    /// Epoch seconds the replacement secret was minted at.
    pub rotated_at: i64,
    /// Epoch seconds the replaced secret had been minted at, if one existed.
    pub replaced_minted_at: Option<i64>,
}

/// **Re-mint** the OAuth session secret, invalidating every outstanding OAuth
/// refresh token at once — the refresh plane's half of the compromise
/// response (`authorization-server.md` § The issuer → *Two rotation arms*;
/// custody and blast radius: `key-material-hierarchy.md` § Audience:
/// deployment infrastructure → *OAuth refresh-token signing secret*).
///
/// There is no ordinary arm here, and that is the shape rather than a gap: the
/// issuer key is a **set** because strangers verify its public half and a
/// rotation must keep the outgoing generation servable; this secret is
/// presented back only to this nest's own `/oauth/token` and `/oauth/revoke`,
/// so a retired generation would have no verifier to satisfy and "keep the
/// old one served for a horizon" means nothing. The only rotation a single-row
/// signer can have is the forced one: every refresh token MACed under the old
/// bytes stops verifying on the next read, and clients re-authorize.
///
/// Why this door exists beside [`crate::oauth_issuer_key::force_rotate`]: the
/// two signers rest in the same store, so a leak that exposes one exposes the
/// other, and a forged refresh token redeems for an access token signed by the
/// *new* issuer key — rotating the issuer key alone leaves the compromise
/// response half done. A deployment-seed rotation does not help either: it
/// re-wraps this row rather than re-minting it, pinned by
/// `a_deployment_seed_rotation_keeps_the_same_secret` below, because losing
/// the secret is not what a seed rotation is for.
///
/// One statement, one row: `INSERT OR REPLACE` on `id = 0` lands the new
/// ciphertext and its mint instant atomically, so no reader can observe a
/// store with no secret or a secret paired with the old instant. The replaced
/// instant is read first so the reply can date what died.
///
/// The seed is read out of `nest_keypair` inside the transaction, never taken
/// from the caller. An admin door is answered by a serving generation, and
/// inside a deployment-seed rotation's hand-off window that generation's copy
/// of the seed is the retired one: a secret sealed under it would never open
/// again, and the satellite walk would refuse it on every later rotation — so
/// a row that already existed wedges the ceremony exactly as a fresh one does.
/// The transaction holds the database guard the rotation holds, so the seed
/// read here is the one the database holds when the row is written.
pub fn force_rotate(conn: &mut Connection) -> Result<ForcedSessionRotation> {
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).context("mint the replacement OAuth session secret")?;
    let rotated_at = crate::db::now_epoch_secs();
    let tx = conn
        .transaction()
        .context("open the forced OAuth session secret rotation")?;
    let deployment_seed = require_deployment_seed(&tx)?;
    let wrapped = wrap_32(OAUTH_SESSION_CONTEXT, &deployment_seed, &secret)
        .context("wrap the replacement OAuth session secret under the deployment seed")?;
    let replaced_minted_at: Option<i64> = tx
        .query_row(
            "SELECT created_at FROM oauth_session_secret WHERE id = 0",
            [],
            |r| r.get(0),
        )
        .optional()
        .context("read the OAuth session secret's mint instant")?;
    tx.execute(
        "INSERT OR REPLACE INTO oauth_session_secret (id, secret_wrapped, created_at)
         VALUES (0, ?1, ?2)",
        rusqlite::params![wrapped, rotated_at],
    )
    .context("replace the OAuth session secret")?;
    tx.commit()
        .context("commit the forced OAuth session secret rotation")?;
    Ok(ForcedSessionRotation {
        rotated_at,
        replaced_minted_at,
    })
}

fn read(conn: &Connection, deployment_seed: &[u8; 32]) -> Result<Option<[u8; 32]>> {
    let wrapped: Option<Vec<u8>> = conn
        .query_row(
            "SELECT secret_wrapped FROM oauth_session_secret WHERE id = 0",
            [],
            |r| r.get(0),
        )
        .optional()
        .context("read the OAuth session secret")?;
    let Some(wrapped) = wrapped else {
        return Ok(None);
    };
    Ok(Some(
        unwrap_32(OAUTH_SESSION_CONTEXT, deployment_seed, &wrapped)
            .context("unwrap the OAuth session secret — wrong deployment seed, or a corrupt row")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE oauth_session_secret (
                 id             INTEGER PRIMARY KEY CHECK(id = 0),
                 secret_wrapped BLOB NOT NULL,
                 created_at     INTEGER NOT NULL
             );
             CREATE TABLE nest_keypair (
                 id          INTEGER PRIMARY KEY CHECK (id = 1),
                 secret_key  BLOB NOT NULL,
                 public_key  BLOB NOT NULL,
                 created_at  INTEGER NOT NULL
             );",
        )
        .expect("schema");
        conn
    }

    fn seed(b: u8) -> [u8; 32] {
        [b; 32]
    }

    /// Seat `seed` as the deployment keypair — what [`force_rotate`] seals
    /// under. Only the seed is read, so the public half is left empty.
    fn hold_deployment_seed(conn: &Connection, seed: &[u8; 32]) {
        conn.execute(
            "INSERT OR REPLACE INTO nest_keypair (id, secret_key, public_key, created_at)
             VALUES (1, ?1, x'', 0)",
            rusqlite::params![seed.as_slice()],
        )
        .expect("seat the deployment keypair");
    }

    fn rows(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM oauth_session_secret", [], |r| {
            r.get(0)
        })
        .expect("count")
    }

    /// The whole fix at its narrowest: an absent row is an answer a read
    /// returns, never a gap it fills.
    #[test]
    fn a_read_never_mints() {
        let conn = db();
        let read = session_secret(&conn, &seed(1));
        assert_eq!(
            rows(&conn),
            0,
            "a read must never mint the OAuth session secret"
        );
        let err = read.expect_err("no row, so no secret");
        assert!(
            err.is::<NotProvisioned>(),
            "an absent row is the named NotProvisioned error, got: {err:#}"
        );
    }

    /// The boot mint is first-write-wins: a second mint leaves the bytes alone.
    /// One that replaced them would silently invalidate every refresh token
    /// already in a client's hands.
    #[test]
    fn the_secret_is_minted_once_and_read_back_stable() {
        let conn = db();
        mint_if_absent(&conn, &seed(1)).expect("mint");
        let first = session_secret(&conn, &seed(1)).expect("read");
        mint_if_absent(&conn, &seed(1)).expect("a second mint");
        let second = session_secret(&conn, &seed(1)).expect("read again");
        assert_eq!(first, second);
        assert_ne!(first, [0u8; 32], "an all-zero secret is not a mint");
        assert_eq!(rows(&conn), 1);
    }

    /// The seal is the point: the row is opened by a key derived from the
    /// deployment seed, so a DB dump without that seed yields nothing. A wrong
    /// seed must be an error rather than different bytes — silently signing
    /// under a secret nobody can verify is worse than refusing — and a mint
    /// under it must refuse rather than write a second secret over the first.
    #[test]
    fn a_wrong_deployment_seed_refuses_rather_than_yielding_other_bytes() {
        let conn = db();
        mint_if_absent(&conn, &seed(1)).expect("mint");
        assert!(session_secret(&conn, &seed(2)).is_err());
        assert!(mint_if_absent(&conn, &seed(2)).is_err());
    }

    /// The stored ciphertext is not the secret. Pinned because the wrap is one
    /// call deep, and a refactor that dropped it would still pass every test
    /// above.
    #[test]
    fn the_row_holds_ciphertext_not_the_secret() {
        let conn = db();
        mint_if_absent(&conn, &seed(1)).expect("mint");
        let secret = session_secret(&conn, &seed(1)).expect("read");
        let stored: Vec<u8> = conn
            .query_row(
                "SELECT secret_wrapped FROM oauth_session_secret WHERE id = 0",
                [],
                |r| r.get(0),
            )
            .expect("row");
        assert_ne!(stored.as_slice(), secret.as_slice());
        assert!(
            !stored.windows(32).any(|w| w == secret),
            "the plaintext secret appears verbatim inside the stored blob"
        );
    }

    /// A deployment-seed rotation must carry this row, not strand it — which
    /// is what the `nest_kek::SATELLITES` registration buys. Pinned end to end
    /// rather than by asserting list membership: the failure this prevents is
    /// the secret becoming unopenable, not a missing table name.
    #[test]
    fn a_deployment_seed_rotation_keeps_the_same_secret() {
        let conn = db();
        mint_if_absent(&conn, &seed(1)).expect("mint");
        let before = session_secret(&conn, &seed(1)).expect("read");
        crate::nest_kek::reencrypt_satellites(&conn, &seed(1), &seed(2)).expect("rotate");
        let after = session_secret(&conn, &seed(2)).expect("read under the new seed");
        assert_eq!(before, after);
    }

    /// The forced rotation is the one door that changes the bytes: a refresh
    /// token MACed under the old secret fails verification under the new one
    /// on the very next read, and the replaced mint instant is reported so the
    /// reply can date what died. Pinned through the real mint/verify pair
    /// rather than by comparing bytes alone, because "the bytes changed" is
    /// not the property an admin is buying — "every outstanding refresh token
    /// is dead" is.
    #[test]
    fn a_forced_rotation_invalidates_every_refresh_token_minted_before_it() {
        use crate::oauth_as_token::{RefreshClaims, mint_refresh_token, verify_refresh_token};
        let mut conn = db();
        hold_deployment_seed(&conn, &seed(1));
        mint_if_absent(&conn, &seed(1)).expect("mint");
        let old = session_secret(&conn, &seed(1)).expect("read");
        let now = 1_700_000_000;
        let claims = RefreshClaims {
            sub: "did:plc:alice".into(),
            aud: "https://nest.example".into(),
            scope: crate::oauth_as_token::SCOPE_REFRESH.into(),
            iat: now,
            exp: now + 3600,
            jti: "j".into(),
            sid: "s".into(),
            ascope: String::new(),
            fauna_actor: String::new(),
            plane: crate::oauth_as_token::PLANE_OAUTH.into(),
            jkt: String::new(),
            client_id: String::new(),
            sexp: 0,
        };
        let token = mint_refresh_token(&old, &claims).expect("mint token");
        assert!(
            verify_refresh_token(&old, &token, "https://nest.example", now + 1).is_some(),
            "precondition: the token verifies under the secret that minted it"
        );

        let forced = force_rotate(&mut conn).expect("force-rotate");
        let new = session_secret(&conn, &seed(1)).expect("read after rotation");
        assert_ne!(old, new, "a forced rotation must re-mint, not re-wrap");
        assert!(
            verify_refresh_token(&new, &token, "https://nest.example", now + 1).is_none(),
            "a refresh token minted before the forced rotation must fail under the \
             replacement secret — that is the whole compromise response"
        );
        assert!(forced.rotated_at > 0);
        assert!(
            forced.replaced_minted_at.is_some(),
            "the reply dates the secret it replaced"
        );

        // And the replacement is what every later read sees — one row, no
        // retired generation kept anywhere.
        assert_eq!(session_secret(&conn, &seed(1)).expect("stable"), new);
    }

    /// A forced rotation on a store the boot step never seated is the same act
    /// as a first mint — the reply simply has nothing to date — so the door has
    /// one code path whether or not a token was ever issued.
    #[test]
    fn a_forced_rotation_on_a_fresh_store_mints_and_reports_nothing_replaced() {
        let mut conn = db();
        hold_deployment_seed(&conn, &seed(1));
        let forced = force_rotate(&mut conn).expect("force-rotate a fresh store");
        assert_eq!(forced.replaced_minted_at, None);
        let stored_at: i64 = conn
            .query_row(
                "SELECT created_at FROM oauth_session_secret WHERE id = 0",
                [],
                |r| r.get(0),
            )
            .expect("the forced rotation itself must have written the row");
        assert_eq!(
            stored_at, forced.rotated_at,
            "the row carries the mint instant it reports"
        );
        let after = session_secret(&conn, &seed(1)).expect("read");
        assert_ne!(after, [0u8; 32]);
    }

    /// The rotated secret is sealed exactly as the first one was: a wrong seed
    /// refuses, and a deployment-seed rotation carries the NEW row.
    #[test]
    fn the_rotated_secret_is_sealed_and_carried_like_the_first() {
        let mut conn = db();
        hold_deployment_seed(&conn, &seed(1));
        mint_if_absent(&conn, &seed(1)).expect("mint");
        force_rotate(&mut conn).expect("force-rotate");
        assert!(
            session_secret(&conn, &seed(2)).is_err(),
            "wrong seed refuses"
        );
        let rotated = session_secret(&conn, &seed(1)).expect("read");
        crate::nest_kek::reencrypt_satellites(&conn, &seed(1), &seed(3)).expect("seed rotation");
        assert_eq!(
            session_secret(&conn, &seed(3)).expect("read under the new seed"),
            rotated
        );
    }

    /// The forced door seals under the seed the database holds and takes none
    /// from its caller — inside a deployment-seed rotation's hand-off window a
    /// serving generation's copy is the retired one. Pinned here at the module;
    /// `oauth_issuer_handlers::tests::rotation_window` drives the same door
    /// through a real ceremony.
    #[test]
    fn a_forced_rotation_seals_under_the_seed_the_database_holds() {
        let mut conn = db();
        hold_deployment_seed(&conn, &seed(1));
        mint_if_absent(&conn, &seed(1)).expect("mint");
        crate::nest_kek::reencrypt_satellites(&conn, &seed(1), &seed(2)).expect("re-key");
        hold_deployment_seed(&conn, &seed(2));

        force_rotate(&mut conn).expect("force-rotate");

        assert!(
            session_secret(&conn, &seed(2)).is_ok(),
            "the replacement is sealed under the seed the database holds"
        );
        assert!(
            session_secret(&conn, &seed(1)).is_err(),
            "and not under the retired one"
        );
    }

    /// A nest with no deployment keypair has nothing to seal under, so the
    /// forced door refuses and writes nothing.
    #[test]
    fn a_forced_rotation_without_a_deployment_keypair_refuses() {
        let mut conn = db();
        assert!(force_rotate(&mut conn).is_err());
        assert_eq!(rows(&conn), 0);
    }
}
