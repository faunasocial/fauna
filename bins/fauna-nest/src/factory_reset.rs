//! Factory reset: return the nest to **fresh / unclaimed** via a restart-wipe.
//!
//! The WS-RPC kind `fauna.admin.factory_reset` (`admin_ws_handlers.rs`) does
//! NOT wipe in-process — truncating tables while the DB is open is incomplete
//! and racy. Instead it **stages a marker** carrying the next claim code, replies
//! to the caller, then exits; the s6 supervisor (`longrun`) restarts the process,
//! and `maybe_run_factory_reset` runs at the very top of startup — *before* the
//! DB is opened — to wipe deployment state and clear the marker.
//!
//! **Wiped** (all deployment state): the SQLite DB (actors, handles, admin
//! grants, mail domains, `account_aliases`, bridge enrollments, mail records,
//! audit log — every table), the blob dir (mail blobs, bridge wrapped-MLS
//! blobs, content blobs), the mail-enable gate, and the claim-code file
//! (regenerated from the marker).
//!
//! **Preserved**: the durable **deployment signing key** (`nest_deployment.key`)
//! — the nest's SINGLE identity (single-identity unification, `box-recovery.md`):
//! the channel-binding `nest_actor_id` a client TOFU-pins / a public domain
//! publishes as DNS `self=`, AND the identity nest.info/federation/backup/pairing/
//! sync key off; it is restored into the freshly-migrated DB by
//! `crate::deployment_key` so a pinned client reconnects to the re-claimed nest
//! instead of failing the TOFU identity check (the legacy separate
//! `nest_identity.key` is retired). The on-disk ACME cert
//! (`acme_dir/*.pem` — a *separate* directory, never touched here) so the box
//! stays reachable over TLS for the re-claim, `nest.toml`, and the bridge
//! keypairs under `keys/`.
//!
//! **Carried**: the DKIM signing keys. They rest in the database this wipe
//! deletes, so the first step copies each active mail domain's active-selector
//! key — sealed, as it rests — into the data dir, and the boot that follows
//! seats them on the fresh database for the door that re-registers the domain
//! to adopt (`crate::mail_dkim_key`, *Carried keys*). The DNS record the admin
//! published stays valid across a reset. Best-effort: a key that cannot be
//! carried is minted afresh when its domain comes back.
//!
//! **Running mail-bridge services are brought DOWN, not left running.** The
//! `fauna-mail-bridge-{mta,mda}` s6 services are long-lived and survive the nest
//! restart; the wipe deletes their enrollment rows. They only `request_enrollment`
//! at cold boot, so a still-running bridge would loop forever on `/auth/verify
//! 404 actor not registered` against the fresh nest (never re-enrolling). So the
//! `fauna.admin.factory_reset` handler signals the supervisor sidekick socket to
//! `s6-svc -d` both role services *before* it exits (same path as
//! `set_mail_enabled(false)` —
//! `mail_enable::reconcile_supervisor(node_mode, false, false, false, false)`).
//! The freshly-claimed nest therefore starts mail-off (matching the default-off
//! invariant); the next `set_mail_enabled(true)` `s6-svc -u`'s the services into
//! a clean cold boot, where they re-`request_enrollment` PENDING against the
//! fresh nest. See `behavior/mail-bridge-lifecycle.md` § Factory reset.
//!
//! ⚠ v1 is `Admin`-gated only. The cooldown / client-compromise gating is a
//! deferred follow-up — acceptable on the disposable dogfood VPS
//! (`dogfood-vps-is-disposable-preprod`). See `behavior/mail-bridge-lifecycle.md`
//! § Factory reset.

use std::io;
use std::path::{Path, PathBuf};

/// Marker filename staged by the handler and consumed at startup. Its contents
/// are the post-reset claim code (so the code survives the wipe and the freshly
/// booted nest accepts it).
const MARKER_NAME: &str = "factory-reset-requested";

/// Resolve the data dir from a configured `db_path` — the parent directory,
/// falling back to `/data`. Mirrors `claim::claim_code_path_for_db`.
pub fn data_dir_for_db(db_path: &str) -> PathBuf {
    Path::new(db_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("/data"))
        .to_path_buf()
}

/// Path to the factory-reset marker for a given data dir.
pub fn marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join(MARKER_NAME)
}

/// Stage a factory reset: write the marker carrying `claim_code`. Called by the
/// handler before it replies + exits. Atomic enough for our purposes (single
/// small write into the persistent data dir); the wipe is idempotent on the
/// contents so a partial write just means a re-run.
pub fn stage_factory_reset(data_dir: &Path, claim_code: &str) -> io::Result<()> {
    std::fs::write(marker_path(data_dir), claim_code.as_bytes())
}

/// Run a pending factory reset if the marker is present. Call at the very top of
/// startup, **before** opening the DB. Returns `Ok(true)` if a reset ran,
/// `Ok(false)` if there was no marker. The wipe deletes deployment state and
/// rewrites the claim-code file from the marker, then removes the marker.
///
/// `db_path` is the SQLite path; `blob_dir` is the blob root. The data dir is
/// derived from `db_path`.
pub fn maybe_run_factory_reset(db_path: &str, blob_dir: &str) -> io::Result<bool> {
    let data_dir = data_dir_for_db(db_path);
    let marker = marker_path(&data_dir);
    let claim_code = match std::fs::read_to_string(&marker) {
        Ok(s) => s.trim().to_string(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };

    tracing::warn!(
        "factory_reset: marker present — wiping deployment state (preserving host \
         identity + ACME cert); fresh claim code staged"
    );

    // 0. Carry the DKIM keys out of the DB before it goes. Never fatal: the
    //    reset is the recovery floor, and a domain whose key was not carried
    //    mints a new one when it is added again.
    match crate::mail_dkim_key::carry_across_factory_reset(Path::new(db_path), &data_dir) {
        Ok(0) => {}
        Ok(n) => tracing::info!("factory_reset: carried {n} DKIM signing key(s) across the wipe"),
        Err(e) => tracing::warn!(
            "factory_reset: could not carry the DKIM signing keys; each mail domain mints a \
             new key when it is added again and its DKIM record must be republished: {e:#}"
        ),
    }

    // 1. SQLite DB + WAL/SHM sidecars. Migrations recreate an empty DB on boot.
    remove_file_if_exists(Path::new(db_path))?;
    remove_file_if_exists(Path::new(&format!("{db_path}-wal")))?;
    remove_file_if_exists(Path::new(&format!("{db_path}-shm")))?;

    // 2. Blob dir contents (mail blobs, bridge wrapped-MLS blobs, content
    //    blobs). Keep the directory itself so the blob store can write into it.
    clear_dir_contents(Path::new(blob_dir))?;

    // 3. Deployment markers in the data dir. The claim-code file is rewritten
    //    below from the marker; imap-enabled / caldav / carddav / webdav /
    //    atproto-enabled → off.
    //
    //    EVERY service-gating flag `mail_enable` writes belongs in this list,
    //    because nothing else clears one: a boot reconcile no-ops while its DB
    //    toggle is unset, which is exactly the post-reset state, and the
    //    atproto flag has no reconcile at all. A leftover file therefore keeps
    //    its service running on a reset box that has no users to serve —
    //    `mda_should_run` ORs the four IMAP/DAV flags, and `atproto-enabled`
    //    gates the `fauna-atproto-bridge` service on its own. Named by
    //    constant, not string literal, so a renamed flag fails the build here
    //    instead of silently dropping out of the reset.
    remove_file_if_exists(&data_dir.join(crate::mail_enable::MAIL_ENABLE_FLAG))?;
    remove_file_if_exists(&data_dir.join(crate::mail_enable::CALDAV_ENABLE_FLAG))?;
    remove_file_if_exists(&data_dir.join(crate::mail_enable::CARDDAV_ENABLE_FLAG))?;
    remove_file_if_exists(&data_dir.join(crate::mail_enable::WEBDAV_ENABLE_FLAG))?;
    remove_file_if_exists(&data_dir.join(crate::mail_enable::ATPROTO_ENABLE_FLAG))?;

    // 4. Stage the post-reset claim code so `ensure_claim_code_at` (later in
    //    startup) preserves it and the freshly-booted nest is claimable with it.
    let claim_path = data_dir.join("claim-code");
    if claim_code.is_empty() {
        // Defensive: an empty marker means "generate a fresh one" — delete the
        // file so `ensure_claim_code_at` mints a new code.
        remove_file_if_exists(&claim_path)?;
    } else {
        std::fs::write(&claim_path, claim_code.as_bytes())?;
    }

    // 5. Drop the marker last, so a crash mid-wipe re-runs the (idempotent) wipe.
    remove_file_if_exists(&marker)?;

    tracing::warn!("factory_reset: wipe complete — booting fresh / unclaimed");
    Ok(true)
}

fn remove_file_if_exists(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Remove every entry inside `dir` (files and subdirs) but keep `dir` itself.
/// A missing dir is a no-op.
fn clear_dir_contents(dir: &Path) -> io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail_enable::{
        ATPROTO_ENABLE_FLAG, CALDAV_ENABLE_FLAG, CARDDAV_ENABLE_FLAG, MAIL_ENABLE_FLAG,
        WEBDAV_ENABLE_FLAG,
    };
    use tempfile::TempDir;

    /// Every service-gating enable flag the reset must clear. Named by
    /// constant so this list and `maybe_run_factory_reset`'s delete list can
    /// only drift by an explicit edit to one of them.
    const ENABLE_FLAGS: [&str; 5] = [
        MAIL_ENABLE_FLAG,
        CALDAV_ENABLE_FLAG,
        CARDDAV_ENABLE_FLAG,
        WEBDAV_ENABLE_FLAG,
        ATPROTO_ENABLE_FLAG,
    ];

    /// Build a fake populated data dir + blob dir, stage a reset, run it, and
    /// assert deployment state is gone while preserved files survive and the
    /// claim code from the marker is installed.
    #[test]
    fn wipe_clears_deployment_state_preserves_identity_and_cert() {
        let dir = TempDir::new().unwrap();
        let data = dir.path();
        let db_path = data.join("nest.db");
        let blob_dir = data.join("blobs");
        let acme_dir = data.join("acme");
        std::fs::create_dir_all(&blob_dir).unwrap();
        std::fs::create_dir_all(&acme_dir).unwrap();
        std::fs::create_dir_all(blob_dir.join("ab")).unwrap();

        // Deployment state (to be wiped).
        std::fs::write(&db_path, b"sqlite").unwrap();
        std::fs::write(data.join("nest.db-wal"), b"wal").unwrap();
        std::fs::write(data.join("nest.db-shm"), b"shm").unwrap();
        std::fs::write(blob_dir.join("ab").join("blob1"), b"blob").unwrap();
        // Every service-gating enable flag, enumerated from `mail_enable`'s own
        // constants so a newly-added flag shows up here as a compile-time
        // decision rather than being silently left out of the reset.
        for flag in ENABLE_FLAGS {
            std::fs::write(data.join(flag), b"").unwrap();
        }
        std::fs::write(data.join("claim-code"), b"OLDOLD").unwrap();

        // Preserved files. `nest_deployment.key` is the nest's single identity
        // (single-identity unification); the wipe must never delete it.
        std::fs::write(
            data.join("nest_deployment.key"),
            b"deploykeydeploykeydeploykeydeplo",
        )
        .unwrap();
        std::fs::write(acme_dir.join("fullchain.pem"), b"cert").unwrap();
        std::fs::write(acme_dir.join("privkey.pem"), b"key").unwrap();

        let db_path_str = db_path.to_str().unwrap();
        let blob_dir_str = blob_dir.to_str().unwrap();

        stage_factory_reset(data, "NEWNEW").unwrap();
        let ran = maybe_run_factory_reset(db_path_str, blob_dir_str).unwrap();
        assert!(ran, "marker present → reset must run");

        // Wiped.
        assert!(!db_path.exists(), "DB must be gone");
        assert!(!data.join("nest.db-wal").exists(), "WAL must be gone");
        assert!(!data.join("nest.db-shm").exists(), "SHM must be gone");
        assert!(!blob_dir.join("ab").exists(), "blob subtree must be gone");
        for flag in ENABLE_FLAGS {
            assert!(
                !data.join(flag).exists(),
                "{flag} must be gone — a leftover enable flag keeps its service \
                 running on a reset box with no users to serve, and nothing else \
                 clears it (a boot reconcile no-ops while the DB toggle is unset, \
                 and the atproto flag has no reconcile at all)"
            );
        }
        assert!(blob_dir.exists(), "blob dir itself must survive for re-use");

        // Claim code replaced by the marker's value.
        assert_eq!(
            std::fs::read_to_string(data.join("claim-code")).unwrap(),
            "NEWNEW"
        );

        // Preserved.
        assert!(
            data.join("nest_deployment.key").exists(),
            "deployment identity (the nest's single identity) preserved"
        );
        assert!(
            acme_dir.join("fullchain.pem").exists(),
            "ACME cert preserved"
        );
        assert!(acme_dir.join("privkey.pem").exists(), "ACME key preserved");

        // Marker cleared → a second startup does not re-wipe.
        assert!(!marker_path(data).exists(), "marker must be cleared");
        assert!(
            !maybe_run_factory_reset(db_path_str, blob_dir_str).unwrap(),
            "no marker → no reset"
        );
    }

    /// One boot of a nest on `data`: open the database, reconcile the
    /// deployment keypair and seat whatever a reset carried — `main`'s order.
    async fn boot(data: &Path) -> crate::db::CacheDb {
        let db = crate::db::CacheDb::open(data.join("nest.db")).unwrap();
        crate::deployment_key::reconcile_deployment_keypair(&db, data, None)
            .await
            .unwrap();
        db.receive_carried_dkim_keys(data).await.unwrap();
        db
    }

    async fn factory_reset(data: &Path) {
        stage_factory_reset(data, "NEWNEW").unwrap();
        let ran = maybe_run_factory_reset(
            data.join("nest.db").to_str().unwrap(),
            data.join("blobs").to_str().unwrap(),
        )
        .unwrap();
        assert!(ran);
    }

    const RAW: &[u8] = b"From: alice@example.test\r\nSubject: hi\r\n\r\nbody\r\n";

    async fn signs(db: &crate::db::CacheDb) -> bool {
        crate::mail_dkim_key::OutboundSigner::load(&*db.conn().await)
            .unwrap()
            .sign(RAW)
            .is_some()
    }

    /// The published DKIM record survives a factory reset: the selector list
    /// (`fauna.bridges.list_dkim_selectors`' one read) is the same before the
    /// reset and after the domain is registered again, the carried key still
    /// signs, and in between the unclaimed box lists nothing.
    #[tokio::test]
    async fn a_factory_reset_carries_the_dkim_key_to_the_re_registered_domain() {
        let dir = TempDir::new().unwrap();
        let data = dir.path();

        let before = {
            let db = boot(data).await;
            db.add_mail_domain("example.test", true, "testing", "none", None, None)
                .await
                .unwrap();
            // A domain that has rotated off the default selector.
            db.add_mail_domain(
                "rotated.test",
                false,
                "testing",
                "none",
                None,
                Some("202609"),
            )
            .await
            .unwrap();
            // A superseded selector's key is not what signs, so it stays behind.
            db.seat_dkim_selector_for_test("rotated.test", "default", "v=DKIM1; k=ed25519; p=OLD")
                .await;
            assert!(signs(&db).await);
            let mut live = db.list_dkim_selectors(None).await.unwrap();
            live.retain(|row| !(row.domain == "rotated.test" && row.selector == "default"));
            assert_eq!(live.len(), 2);
            live
        };

        factory_reset(data).await;
        let db = boot(data).await;
        assert!(
            db.list_dkim_selectors(None).await.unwrap().is_empty(),
            "a carried key is no selector until its domain is registered again"
        );
        assert!(
            !data.join("dkim-keys-carried").exists(),
            "the carry file is consumed by the boot that seats it"
        );

        db.add_mail_domain("example.test", true, "testing", "none", None, None)
            .await
            .unwrap();
        let rotated = db
            .add_mail_domain("rotated.test", false, "testing", "none", None, None)
            .await
            .unwrap();
        assert_eq!(
            rotated.dkim_selector.as_deref(),
            Some("202609"),
            "the domain comes back on the selector its DNS still publishes"
        );
        assert_eq!(
            db.list_dkim_selectors(None).await.unwrap(),
            before,
            "same selectors, same public records, same mint times"
        );
        assert!(signs(&db).await, "and the carried key opens and signs");
    }

    /// A domain nobody registers again leaves a carried key that the mail-domain
    /// GC removes on the soft-delete window; a different domain mints its own.
    #[tokio::test]
    async fn an_unclaimed_carried_key_is_never_listed() {
        let dir = TempDir::new().unwrap();
        let data = dir.path();
        {
            let db = boot(data).await;
            db.add_mail_domain("example.test", true, "testing", "none", None, None)
                .await
                .unwrap();
        }
        factory_reset(data).await;
        let db = boot(data).await;
        db.add_mail_domain("other.test", true, "testing", "none", None, None)
            .await
            .unwrap();
        let listed = db.list_dkim_selectors(None).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].domain, "other.test");

        let conn = db.conn().await;
        let carried = |conn: &rusqlite::Connection| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM mail_dkim_keys WHERE carried_at IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(carried(&conn), 1);
        assert_eq!(
            crate::mail_dkim_key::expire_carried(&conn, 0).unwrap(),
            0,
            "inside the window it is kept"
        );
        assert_eq!(
            crate::mail_dkim_key::expire_carried(&conn, i64::MAX).unwrap(),
            1
        );
        assert_eq!(carried(&conn), 0);
    }

    /// A second run of the wipe after the database is already gone (a crash
    /// before the marker cleared) keeps what the first run carried.
    #[tokio::test]
    async fn a_rerun_of_the_wipe_keeps_the_carried_keys() {
        let dir = TempDir::new().unwrap();
        let data = dir.path();
        let before = {
            let db = boot(data).await;
            db.add_mail_domain("example.test", true, "testing", "none", None, None)
                .await
                .unwrap();
            db.list_dkim_selectors(None).await.unwrap()
        };
        factory_reset(data).await;
        factory_reset(data).await;
        let db = boot(data).await;
        db.add_mail_domain("example.test", true, "testing", "none", None, None)
            .await
            .unwrap();
        assert_eq!(db.list_dkim_selectors(None).await.unwrap(), before);
    }

    #[test]
    fn no_marker_is_a_noop() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("nest.db");
        std::fs::write(&db_path, b"sqlite").unwrap();
        let ran = maybe_run_factory_reset(
            db_path.to_str().unwrap(),
            dir.path().join("blobs").to_str().unwrap(),
        )
        .unwrap();
        assert!(!ran, "no marker → no reset");
        assert!(db_path.exists(), "DB untouched when no marker");
    }

    #[test]
    fn empty_marker_regenerates_claim_code() {
        let dir = TempDir::new().unwrap();
        let data = dir.path();
        let db_path = data.join("nest.db");
        std::fs::write(&db_path, b"sqlite").unwrap();
        std::fs::write(data.join("claim-code"), b"OLDOLD").unwrap();
        // Empty marker = "generate a fresh code on boot".
        stage_factory_reset(data, "").unwrap();
        maybe_run_factory_reset(
            db_path.to_str().unwrap(),
            data.join("blobs").to_str().unwrap(),
        )
        .unwrap();
        assert!(
            !data.join("claim-code").exists(),
            "empty marker deletes claim-code so ensure_claim_code_at mints a new one"
        );
    }
}
