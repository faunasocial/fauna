//! **tier_3** — the Nostr legs of identity succession (`docs/goal/ui/nostr.md`
//! § Key succession and rotation, ratified 2026-08-11; summary row in
//! `succession-aftermath.md` § Re-key scope). Real `CacheDb`, real schema, the
//! real `record_succession` transaction — no mocks.
//!
//! **What these tests are for.** The ceremony's whole purpose is to end a theft,
//! and until this landed it ended it only *partly*: `record_succession` touched
//! no `nostr_*` table, so a thief-authorized NIP-46 connection kept signing as
//! the user's npub after the succession, and the successor's Nostr page read
//! *unlinked* while the deposited key sat stranded on the refused identity.
//! So every test below asks one of two questions — does authority the thief
//! could plant actually die, and does the user's own Nostr identity actually
//! arrive?
//!
//! ⚠ **The trap this file exists to keep closed:** every `nostr_*` table keys
//! its actor as lowercase **hex TEXT**, while the succession transaction binds
//! the raw 32-byte **BLOB** everywhere else. SQLite never matches a blob
//! parameter against a TEXT column, so the wrong encoding is not a type error —
//! it is a statement that updates zero rows and looks exactly like an account
//! with no Nostr link. Each assertion below is written against the *hex* key for
//! that reason.

#![cfg(feature = "nostr")]

use std::sync::Arc;

use fauna_bridge_nostr::nip46::{Nip46Method, Nip46Request};
use fauna_nest::db::CacheDb;
use fauna_nest::nostr::bunker;
use fauna_nest::nostr::db as nostr_db;

const OLD: [u8; 32] = [0x11; 32];
const NEW: [u8; 32] = [0x22; 32];
const NPUB: &str = "npub_pubkey_hex_of_the_deposited_key";
/// The thief's own unredeemed invite secret, seeded by `nest_with_a_linked_account`.
const PENDING_INVITE_SECRET: &str = "the-thief-minted-this-invite-and-never-redeemed-it";

fn hex_of(actor: [u8; 32]) -> String {
    hex::encode(actor)
}

/// A nest whose Nostr bridge has run: the tables exist, `old` holds a linked
/// account with a deposited key, one live bunker connection, a bunker signer
/// keypair, a designated zap signer and a follow.
async fn nest_with_a_linked_account() -> Arc<CacheDb> {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    db.create_user_with_handle(&OLD, "free", "alice", None)
        .await
        .unwrap();
    fauna_nest::nostr::init_db(&db).await.unwrap();

    let conn = db.conn().await;
    let old = hex_of(OLD);
    nostr_db::link_account(
        &conn,
        &old,
        NPUB,
        "generated",
        Some(b"the deposited nsec, sealed under the nest identity key"),
        None,
        None,
    )
    .unwrap();
    conn.execute(
        "INSERT INTO nostr_bunker_signers (actor_id, signer_pubkey, encrypted_privkey, created_at)
         VALUES (?1, 'signer-pubkey', X'00', 0)",
        rusqlite::params![old],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO nostr_bunker_apps
             (actor_id, app_pubkey, label, status, secret_hash, created_at, expires_at)
         VALUES (?1, 'thief-app-pubkey', 'the thief''s app', 'connected', X'01', 0, 9999999999)",
        rusqlite::params![old],
    )
    .unwrap();
    // An unredeemed invite the thief minted but never claimed — a `connect`
    // hasn't happened yet, so `app_pubkey` is still NULL (`create_invite`'s
    // own row shape). Leg 2's revoke is status-agnostic, so this row must die
    // exactly like the connected one — a thief holding only the secret, never
    // having connected, is the class row 83 was filed for.
    conn.execute(
        "INSERT INTO nostr_bunker_apps
             (actor_id, app_pubkey, label, status, secret_hash, created_at, expires_at)
         VALUES (?1, NULL, '', 'pending', ?2, 0, 9999999999)",
        rusqlite::params![
            old,
            blake3::hash(PENDING_INVITE_SECRET.as_bytes())
                .as_bytes()
                .to_vec()
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO nostr_zap_signers (actor_id, signer_pubkey, label, created_at)
         VALUES (?1, 'thief-zap-signer', 'planted', 0)",
        rusqlite::params![old],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO nostr_follows (actor_id, nostr_pubkey, created_at) VALUES (?1, 'friend', 0)",
        rusqlite::params![old],
    )
    .unwrap();
    drop(conn);
    db
}

async fn succeed(db: &CacheDb) {
    db.record_succession(&OLD[..], &NEW[..], b"statement", 2)
        .await
        .unwrap()
        .expect("the succession applies");
}

async fn count(db: &CacheDb, sql: &str, actor: [u8; 32]) -> i64 {
    let conn = db.conn().await;
    conn.query_row(sql, rusqlite::params![hex_of(actor)], |r| r.get(0))
        .unwrap()
}

/// The same read with no actor bound — "under **either** actor id", which is the
/// assertion that separates a real revocation from a row that merely moved.
async fn count_all(db: &CacheDb, sql: &str) -> i64 {
    let conn = db.conn().await;
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// **Leg 2, the live hole this row was filed for.** A bunker connection is
/// standing third-party authority: it signs as the user's npub, silently, with
/// no further consent. A thief who minted one during the compromise window kept
/// it *after* the ceremony that was supposed to end the theft — the ceremony
/// revoked sessions, tokens and grants and left this one door open.
///
/// Revocation is wholesale and carries no reason, uniform for theft and loss
/// (`nostr.md:72`): a self-declared reason could not be trusted exactly where it
/// matters.
#[tokio::test]
async fn every_bunker_connection_is_revoked_by_the_succession() {
    let db = nest_with_a_linked_account().await;
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM nostr_bunker_apps WHERE actor_id = ?1 AND status = 'connected'",
            OLD
        )
        .await,
        1,
        "the connection is live before the ceremony"
    );

    succeed(&db).await;

    assert_eq!(
        count_all(
            &db,
            "SELECT COUNT(*) FROM nostr_bunker_apps WHERE status = 'connected'"
        )
        .await,
        0,
        "no bunker connection may survive a succession, under either actor id"
    );
    // …and the row is the successor's to see, not residue stranded on a dead
    // identity: the Connected-apps page is per actor, and what the successor
    // most needs to see is which apps held standing authority.
    let conn = db.conn().await;
    let (owner, status, secret): (String, String, Option<String>) = conn
        .query_row(
            "SELECT actor_id, status, secret_hash FROM nostr_bunker_apps WHERE app_pubkey = 'thief-app-pubkey'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(owner, hex_of(NEW));
    assert_eq!(status, "revoked");
    assert_eq!(secret, None, "the connection secret is cleared, not kept");
}

/// **The gap row 83 was filed for.** Leg 2's revoke is `WHERE actor_id = ?1`
/// with no `status` filter, so it reads as covering a `pending` invite too —
/// but `conformance_succession_nostr.rs` used to seed only a `connected` row,
/// so nothing here ever exercised that. A thief who minted an invite and
/// never redeemed it holds only the secret, not yet a connection; this test
/// is the row-level assertion *and* the behavioral one — the DB row alone
/// proves nothing if the actual redeem path still accepts the secret.
#[tokio::test]
async fn a_pending_bunker_invite_does_not_survive_the_succession() {
    let db = nest_with_a_linked_account().await;
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM nostr_bunker_apps WHERE actor_id = ?1 AND status = 'pending'",
            OLD
        )
        .await,
        1,
        "the invite is live, unredeemed, before the ceremony"
    );

    succeed(&db).await;

    let conn = db.conn().await;
    let (owner, status, secret): (String, String, Option<Vec<u8>>) = conn
        .query_row(
            "SELECT actor_id, status, secret_hash FROM nostr_bunker_apps WHERE app_pubkey IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        owner,
        hex_of(NEW),
        "the row moves to the successor, not left behind"
    );
    assert_eq!(
        status, "revoked",
        "a pending invite is revoked exactly like a connected one"
    );
    assert_eq!(secret, None, "the redeemable secret is cleared, not kept");

    // The behavioral half: the thief's secret, presented through the real
    // redeem path, must be refused — a passing row-level assert alone cannot
    // rule out a second, unpatched read of the pre-succession secret_hash.
    let req = Nip46Request {
        id: "thief-connect-attempt".to_string(),
        method: Nip46Method::Connect,
        params: vec![
            "signer-pubkey".to_string(),
            PENDING_INVITE_SECRET.to_string(),
        ],
    };
    let response_json = bunker::handle_request(
        &conn,
        &[0u8; 32],
        "signer-pubkey",
        "thief-app-pubkey",
        &req,
        0,
    )
    .expect("handle_request answers rather than erroring");
    let response: serde_json::Value = serde_json::from_str(&response_json).unwrap();
    assert_eq!(
        response.get("error").and_then(|e| e.as_str()),
        Some("unauthorized"),
        "the ratified `ui/nostr.md:72` claim — every outstanding connect string dies \
         with its connection — must hold against the real redeem path, not just the row: {response}"
    );
}

/// **Leg 1 — the npub survives and arrives.** The deposited nsec rests under the
/// *nest identity key*, not under anything seed-derived, and crosses no wire
/// outbound: the thief gained use, never possession (`nostr.md:67`). So the
/// account is re-pointed rather than burned — the exact inverse of the MSEK.
#[tokio::test]
async fn the_account_row_re_points_to_the_successor_with_its_key_intact() {
    let db = nest_with_a_linked_account().await;
    succeed(&db).await;

    let conn = db.conn().await;
    let account = nostr_db::get_account(&conn, &hex_of(NEW))
        .unwrap()
        .expect("the successor holds the Nostr account");
    assert_eq!(account.nostr_pubkey, NPUB, "the npub survives the ceremony");
    assert!(
        account.encrypted_privkey.is_some(),
        "the deposited key travels with the account — a re-point, not a burn"
    );
    assert!(
        nostr_db::get_account(&conn, &hex_of(OLD))
            .unwrap()
            .is_none(),
        "nothing is left on the refused identity"
    );
}

/// The satellites follow the account, and this is not tidiness. Both
/// `trusted_zap_signers_for_pubkey` and the follows read JOIN `nostr_accounts`
/// on `actor_id`, so an account re-pointed *alone* would leave the successor
/// with a working npub and silently empty reads — a failure with no error
/// anywhere. The bunker **signer keypair** is in this set deliberately: it never
/// left the box and identifies the signer rather than the user, which is the one
/// thing succession does differently from `unlink_account`'s cascade (that one
/// drops it).
#[tokio::test]
async fn the_actor_keyed_satellites_travel_with_the_account() {
    let db = nest_with_a_linked_account().await;
    succeed(&db).await;

    for table in ["nostr_bunker_signers", "nostr_follows"] {
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE actor_id = ?1");
        assert_eq!(
            count(&db, &sql, NEW).await,
            1,
            "{table} follows the account"
        );
        assert_eq!(
            count(&db, &sql, OLD).await,
            0,
            "{table} leaves nothing on the refused identity"
        );
    }
}

/// **Leg 2's companion (extended from the same principle, 2026-08-11).** A
/// designated zap signer is standing third-party authority of exactly the bunker
/// class: this nest *believes* receipts that signer produces for the user's
/// pubkey, and a believed receipt meeting a tier's asking price buys the post.
/// A thief-planted designation surviving the ceremony would outlive the theft it
/// was supposed to end — so designations are revoked, and re-designating is the
/// same bounded, visible act re-authorizing an app is.
#[tokio::test]
async fn a_planted_zap_signer_designation_does_not_survive_the_succession() {
    let db = nest_with_a_linked_account().await;
    succeed(&db).await;

    assert_eq!(
        count_all(&db, "SELECT COUNT(*) FROM nostr_zap_signers").await,
        0,
        "no zap-signer designation survives, under either actor id"
    );
    // The read the ingest path actually uses agrees — the pin is on the
    // behaviour, not only on the row count.
    let conn = db.conn().await;
    assert!(
        nostr_db::trusted_zap_signers_for_pubkey(&conn, NPUB)
            .unwrap()
            .is_empty(),
        "the ingest-side trust root is empty until the successor designates again"
    );
}

/// **The beside-control, and it is the one that would have caught a wrong
/// encoding as loudly as a wrong table.** A nest whose Nostr bridge never ran
/// has none of these tables, and a succession there must still apply — an
/// unconditional statement against a missing table aborts the whole transaction,
/// which would turn "this nest has no Nostr bridge" into "this nest cannot
/// perform a succession at all".
#[tokio::test]
async fn a_succession_applies_on_a_nest_that_has_no_nostr_tables_at_all() {
    let db = CacheDb::open_in_memory().unwrap();
    db.create_user_with_handle(&OLD, "free", "alice", None)
        .await
        .unwrap();

    db.record_succession(&OLD[..], &NEW[..], b"statement", 2)
        .await
        .unwrap()
        .expect("a nest with no Nostr bridge still succeeds an identity");

    assert!(
        db.succession_for(&OLD[..]).await.unwrap().is_some(),
        "the succession is recorded"
    );
}
