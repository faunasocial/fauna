//! The media proxy's **playback-ticket secret** — the HMAC-SHA-256 key a
//! playback ticket on `GET /api/v1/media/proxy` is signed under.
//!
//! `docs/goal/architecture/key-material-hierarchy.md` § Audience: deployment
//! infrastructure → *Media playback-ticket secret* owns the custody entry;
//! `docs/goal/architecture/render-model.md` § D6c → *Inline playback*, answer
//! (4), owns why the ticket exists. The signing and verifying live in
//! [`crate::media_ticket`]; this module only seats and reads the key.
//!
//! It is the OAuth refresh-token secret's exact shape
//! ([`crate::oauth_session_secret`]): 32 random bytes, one row, sealed under the
//! deployment seed with its own dated context ([`MEDIA_TICKET_CONTEXT`]),
//! registered in `nest_kek::SATELLITES` so a deployment-seed rotation re-wraps
//! it. Not the same key: one key, one purpose, so a leaked ticket is never a
//! refresh-token forgery oracle. And no forced-rotation door: losing the key
//! only invalidates outstanding tickets (a viewer re-taps), so nothing needs
//! one.
//!
//! # When it mints — at boot, never on a read
//!
//! [`mint_if_absent`] seats the row, and the boot step
//! ([`crate::nest_kek::mint_at_boot`]) is its only caller. [`ticket_secret`]
//! only looks the row up — an absent row is [`NotProvisioned`]. So nothing
//! seals this row under a serving generation's copy of the seed, which inside a
//! deployment-seed rotation's hand-off window is the retired one
//! (`key-material-hierarchy.md` § Audience: deployment infrastructure →
//! *Room-read keypair* → *When it mints*).

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension as _};

use crate::nest_kek::{MEDIA_TICKET_CONTEXT, NotProvisioned, unwrap_32, wrap_32};

/// The playback-ticket signing secret — a lookup, never a mint.
///
/// An absent row is [`NotProvisioned`]. A row that does not open under
/// `deployment_seed` is an error too, so a caller holding a stale seed gets a
/// refusal, never a secret.
pub fn ticket_secret(conn: &Connection, deployment_seed: &[u8; 32]) -> Result<[u8; 32]> {
    read(conn, deployment_seed)?
        .ok_or_else(|| NotProvisioned("media playback-ticket secret").into())
}

/// Mint the secret if the row is absent, first-write-wins. Only the boot step
/// ([`crate::nest_kek::mint_at_boot`]) calls it.
///
/// `INSERT OR IGNORE` on the single `id = 0` row and a read-back, so two
/// concurrent mints race harmlessly. A row that does not open under
/// `deployment_seed` refuses rather than being minted over.
pub(crate) fn mint_if_absent(conn: &Connection, deployment_seed: &[u8; 32]) -> Result<()> {
    if read(conn, deployment_seed)?.is_some() {
        return Ok(());
    }
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).context("mint the media playback-ticket secret")?;
    let wrapped = wrap_32(MEDIA_TICKET_CONTEXT, deployment_seed, &secret)
        .context("wrap the media playback-ticket secret under the deployment seed")?;
    conn.execute(
        "INSERT OR IGNORE INTO media_ticket_secret (id, secret_wrapped, created_at)
         VALUES (0, ?1, ?2)",
        rusqlite::params![wrapped, crate::db::now_epoch_secs()],
    )
    .context("insert the media playback-ticket secret")?;
    read(conn, deployment_seed)?.context(
        "the media playback-ticket secret vanished between mint and read-back — refusing to \
         sign tickets under a secret the database never kept",
    )?;
    Ok(())
}

fn read(conn: &Connection, deployment_seed: &[u8; 32]) -> Result<Option<[u8; 32]>> {
    let wrapped: Option<Vec<u8>> = conn
        .query_row(
            "SELECT secret_wrapped FROM media_ticket_secret WHERE id = 0",
            [],
            |r| r.get(0),
        )
        .optional()
        .context("read the media playback-ticket secret")?;
    let Some(wrapped) = wrapped else {
        return Ok(None);
    };
    Ok(Some(
        unwrap_32(MEDIA_TICKET_CONTEXT, deployment_seed, &wrapped).context(
            "unwrap the media playback-ticket secret — wrong deployment seed, or a corrupt row",
        )?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE media_ticket_secret (
                 id             INTEGER PRIMARY KEY CHECK(id = 0),
                 secret_wrapped BLOB NOT NULL,
                 created_at     INTEGER NOT NULL
             );",
        )
        .expect("schema");
        conn
    }

    fn seed(b: u8) -> [u8; 32] {
        [b; 32]
    }

    fn rows(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM media_ticket_secret", [], |r| r.get(0))
            .expect("count")
    }

    /// An absent row is an answer a read returns, never a gap it fills.
    #[test]
    fn a_read_never_mints() {
        let conn = db();
        let read = ticket_secret(&conn, &seed(1));
        assert_eq!(rows(&conn), 0, "a read must never mint the ticket secret");
        let err = read.expect_err("no row, so no secret");
        assert!(
            err.is::<NotProvisioned>(),
            "an absent row is the named NotProvisioned error, got: {err:#}"
        );
    }

    /// First-write-wins: a second mint leaves the bytes alone, so a restart
    /// never invalidates the tickets already in players' hands.
    #[test]
    fn the_secret_is_minted_once_and_read_back_stable() {
        let conn = db();
        mint_if_absent(&conn, &seed(1)).expect("mint");
        let first = ticket_secret(&conn, &seed(1)).expect("read");
        mint_if_absent(&conn, &seed(1)).expect("a second mint");
        assert_eq!(ticket_secret(&conn, &seed(1)).expect("read again"), first);
        assert_ne!(first, [0u8; 32], "an all-zero secret is not a mint");
        assert_eq!(rows(&conn), 1);
    }

    /// The row is ciphertext under the deployment seed: a wrong seed refuses
    /// (and refuses to mint over), and the plaintext never rests in the row.
    #[test]
    fn the_row_is_sealed_under_the_deployment_seed() {
        let conn = db();
        mint_if_absent(&conn, &seed(1)).expect("mint");
        assert!(ticket_secret(&conn, &seed(2)).is_err());
        assert!(mint_if_absent(&conn, &seed(2)).is_err());
        let secret = ticket_secret(&conn, &seed(1)).expect("read");
        let stored: Vec<u8> = conn
            .query_row(
                "SELECT secret_wrapped FROM media_ticket_secret WHERE id = 0",
                [],
                |r| r.get(0),
            )
            .expect("row");
        assert!(
            !stored.windows(32).any(|w| w == secret),
            "the plaintext secret appears verbatim inside the stored blob"
        );
    }

    /// A deployment-seed rotation carries the row rather than stranding it —
    /// the `nest_kek::SATELLITES` registration, pinned end to end.
    #[test]
    fn a_deployment_seed_rotation_keeps_the_same_secret() {
        let conn = db();
        mint_if_absent(&conn, &seed(1)).expect("mint");
        let before = ticket_secret(&conn, &seed(1)).expect("read");
        crate::nest_kek::reencrypt_satellites(&conn, &seed(1), &seed(2)).expect("rotate");
        assert_eq!(
            ticket_secret(&conn, &seed(2)).expect("read under the new seed"),
            before
        );
    }
}
