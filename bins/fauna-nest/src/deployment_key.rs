//! Durable home for the nest's **Deployment Ed25519 signing key**.
//!
//! The deployment signing key (the `nest_keypair` DB row, surfaced as
//! `AppState.nest_signing_key`) is the identity a client **TOFU-pins** via the
//! TLS channel binding, and a public domain publishes as DNS `self=<actor_id>`
//! (`docs/goal/architecture/security.md` § Transport trust, Axis 1/2). The
//! key-material hierarchy treats it as a *root* that "rotates only on a deliberate
//! admin event" (`docs/goal/architecture/key-material-hierarchy.md` § Roots), so
//! it MUST be stable across a **factory reset** — which wipes `nest.db` (and with
//! it the migration-seeded `nest_keypair`) and otherwise regenerates a fresh
//! random deployment identity. A pinning client then sees the new `nest_actor_id`,
//! rejects the channel binding as a TOFU identity change, and never re-establishes
//! its WS-RPC connection to the re-claimed nest (no TLS error — the failure is the
//! channel-binding identity check). That is a client-state recoverability gap
//! (`docs/goal/architecture/nest/common.md` § Client-state recoverability): the
//! box must preserve its deployment identity (and ACME cert) across a reset for
//! exactly this "stays reachable for the re-claim" reason.
//!
//! This module gives the deployment key a durable file home,
//! `nest_deployment.key` (a raw 32-byte Ed25519 seed in the data dir), which
//! `factory_reset::maybe_run_factory_reset` never deletes, and reconciles the DB
//! `nest_keypair` row from it at boot. So a re-claimed nest re-presents the
//! **same** `nest_actor_id` and pinned clients reconnect without a spurious
//! identity-changed failure. This deployment key is the nest's **single**
//! identity (single-identity unification, `box-recovery.md`): `start_server`
//! derives `nest_identity` from the reconciled seed, so nest.info / federation /
//! backup-destination / pairing / sync all key off it (the legacy separate
//! `nest_identity.key` is retired).

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use ed25519_dalek::SigningKey;
use zeroize::Zeroizing;

use crate::db::CacheDb;

/// Filename of the durable deployment-key seed in the nest data dir. Preserved
/// across a factory-reset wipe (it is not in `factory_reset`'s delete list) — it
/// is the nest's single identity, so the wipe must keep it.
const DEPLOYMENT_KEY_NAME: &str = "nest_deployment.key";

/// Path to the durable deployment-key seed for a given data dir.
pub fn deployment_key_path(data_dir: &Path) -> PathBuf {
    data_dir.join(DEPLOYMENT_KEY_NAME)
}

/// Write `bytes` to `path` as an owner-only (`0o600`) file (BR-4
/// defense-in-depth) — the crate-wide pattern for a boot-minted secret (the
/// deployment seed, sidecar tokens, the web-serve holder seed). Lifted to
/// `fauna_core::secret_file::write_secret_file_0600` when `fauna-iroh-relay` needed the
/// identical shape for its relay X25519 key — see that module's doc comment
/// for the crash-atomicity + fresh-by-construction reasoning. Re-exported
/// under the crate-local name so existing call sites are unchanged.
pub(crate) use fauna_core::secret_file::write_secret_file_0600;

/// Env name carrying a **caller-supplied deployment seed** for a box that is being
/// provisioned (or *re-*provisioned, for total-box-loss recovery) with a chosen
/// identity rather than minting its own. Bucket-2 IPC, **artifact-set, never a
/// human-edited knob**: cloud-init env for a client-provisioned cloud
/// VPS, installer input for self-hosted — the admin's client generates the seed,
/// custodies it off-box, and injects it here at provision so the rebuilt box
/// re-presents the **same `nest_actor_id`** (`docs/goal/architecture/nest/box-recovery.md`
/// § Mechanism — "provision a box with a caller-supplied deployment seed").
const DEPLOYMENT_SEED_ENV: &str = "FAUNA_DEPLOYMENT_SEED";

/// Decode a `FAUNA_DEPLOYMENT_SEED` value: a 64-char hex string of the raw 32-byte
/// Ed25519 deployment seed (hex per the repo's `[u8; 32]` key-material convention,
/// e.g. `auth.rs`'s `hex::encode(actor_id)`). Returns `None` for any malformed
/// value (wrong length, non-hex) so a bad env never wedges boot — the box falls
/// back to minting its own identity and logs loudly.
fn decode_deployment_seed(value: &str) -> Option<Zeroizing<[u8; 32]>> {
    let trimmed = value.trim();
    // The intermediate hex-decoded buffer holds the raw seed — zeroize it on drop
    // too: the seed's in-memory holdings zeroize uniformly.
    let bytes = Zeroizing::new(hex::decode(trimmed).ok()?);
    let arr: [u8; 32] = bytes.as_slice().try_into().ok()?;
    Some(Zeroizing::new(arr))
}

/// Read + decode the optional caller-supplied deployment seed from
/// `FAUNA_DEPLOYMENT_SEED`. Absent ⇒ `None` (the box mints/keeps its own identity,
/// the steady-state non-recovery path). A *present but malformed* value is a
/// provisioning misconfiguration: we log an error and return `None` rather than
/// fail to boot.
pub fn deployment_seed_from_env() -> Option<Zeroizing<[u8; 32]>> {
    let raw = Zeroizing::new(std::env::var(DEPLOYMENT_SEED_ENV).ok()?);
    if raw.trim().is_empty() {
        return None;
    }
    match decode_deployment_seed(&raw) {
        Some(seed) => Some(seed),
        None => {
            tracing::error!(
                "{DEPLOYMENT_SEED_ENV} is set but malformed (expected 64 hex chars of a raw \
                 32-byte Ed25519 seed); ignoring it — the box will mint/keep its own \
                 deployment identity"
            );
            None
        }
    }
}

/// Reconcile the DB `nest_keypair` row against the durable `nest_deployment.key`
/// file (the source of truth). Call once at boot, right after the DB is opened
/// (migrations may have just inserted a fresh random row).
///
/// `seed_override` is the optional caller-supplied deployment seed from
/// `FAUNA_DEPLOYMENT_SEED` (see [`deployment_seed_from_env`]) — the
/// "provision a box with a caller-supplied deployment seed" primitive that backs
/// both first provision of a client-provisioned box and **total-box-loss
/// recovery** (`docs/goal/architecture/nest/box-recovery.md` § Mechanism). It only
/// takes effect when the box is **establishing its identity for the first time on
/// this disk** (the durable file is absent); once a box has booted with an
/// identity, that on-disk identity wins and a (possibly stale) env seed is ignored
/// — the deployment seed "rotates only on a deliberate admin event, never on a
/// reset" (`key-material-hierarchy.md`).
///
/// **Rotation-awareness (`box-recovery.md` § The ceremony).** "On-disk wins"
/// holds only for keys the rotation log does **not** supersede. The ceremony
/// commits its transaction and *then* rewrites the file; a crash in between
/// leaves a superseded key on disk with the successor in the DB, and the naive
/// rule would silently un-rotate the box — resurrecting exactly the key the
/// rotation evicted. So a key found in `nest_rotation_log.old_actor_id` loses to
/// the DB, and the file is rewritten forward. The same guard covers a
/// `seed_override` naming a superseded identity (a re-provision env seed landing
/// beside a restored, already-rotated DB).
///
/// - **file present, well-formed (32 bytes)** → the established on-disk
///   identity wins **unless the rotation log supersedes it** (above).
///   Overwrite the DB row with the file's key (after a factory-reset wipe the
///   migration seeded a *new random* key; this restores the preserved
///   deployment identity so the channel-binding `nest_actor_id` stays
///   stable). A `seed_override` that *disagrees* with the established
///   identity is ignored with a loud warning (it is provision-time input,
///   inert once the box has an identity).
/// - **file present, malformed (`len != 32`)** → heal it from the DB
///   `nest_keypair` row if one exists and is itself well-formed (the row
///   stayed authoritative through whatever damaged the file); hard-bail only
///   when there is nothing to heal from ([`heal_malformed_file_or_bail`]).
///   `write_secret_file_0600`'s temp-file-then-rename means this shape can no
///   longer arise from a crash during a rewrite this code performs — it
///   covers a file already malformed before that fix, or external damage.
/// - **file absent + `seed_override`** → install the caller-supplied seed as the
///   box identity (write file + DB row, overwriting any migration-seeded random
///   row). This is the provision-with-identity / recovery path: the rebuilt box
///   re-presents the saved `nest_actor_id`.
/// - **file absent, no override** → adopt the DB row's key into the file (the
///   fresh-box first boot: genesis seeds the row, this persists it — **no key
///   change**), so it becomes durable from here on.
/// - **neither** → generate, write both (defensive; migrations normally seed the
///   DB row so this is unreachable in practice).
pub async fn reconcile_deployment_keypair(
    db: &CacheDb,
    data_dir: &Path,
    seed_override: Option<Zeroizing<[u8; 32]>>,
) -> Result<()> {
    let path = deployment_key_path(data_dir);

    if path.exists() {
        // Hold the on-disk seed (and the read buffer) zeroizing — wipe on
        // drop.
        let bytes = Zeroizing::new(std::fs::read(&path)?);
        if bytes.len() != 32 {
            // write_secret_file_0600's temp-file-then-rename makes a short or
            // empty file impossible from a crash during a FUTURE rewrite —
            // this branch exists for a file already malformed from before
            // that fix landed, or from external damage (disk error, a
            // truncated backup restore, manual tampering). The DB row is
            // still authoritative for exactly the same reason § Rotation-
            // awareness above trusts it after a crashed rotation: heal the
            // file from it rather than bailing the boot over bytes that are
            // recoverable right here.
            return heal_malformed_file_or_bail(db, &path, bytes.len()).await;
        }
        let secret = Zeroizing::new(<[u8; 32]>::try_from(&bytes[..]).expect("len checked == 32"));
        // The box already has an established identity on disk. A caller-supplied
        // FAUNA_DEPLOYMENT_SEED is provision-time input — inert here. If it
        // *disagrees*, surface it loudly: it usually means a re-provision env
        // landed on a surviving disk, or two different seeds were configured.
        if let Some(ov) = &seed_override
            && **ov != *secret
        {
            tracing::warn!(
                "{DEPLOYMENT_SEED_ENV} differs from the established on-disk deployment identity \
                 ({}); keeping the on-disk identity — the env seed is provision-time input, \
                 ignored once the box has booted with an identity",
                DEPLOYMENT_KEY_NAME
            );
        }
        let public = SigningKey::from_bytes(&secret).verifying_key().to_bytes();

        // Rotation-awareness: a superseded on-disk key means the rotation
        // transaction committed and the post-commit file rewrite did not. The DB
        // decision stands; heal the file forward.
        if superseded_by_rotation(db, &public).await? {
            let (db_secret, db_public) = db.get_nest_keypair().await?.ok_or_else(|| {
                anyhow::anyhow!("rotation log is non-empty but no nest_keypair row")
            })?;
            if db_secret.len() != 32 {
                bail!(
                    "DB nest_keypair secret has wrong size: expected 32, got {}",
                    db_secret.len()
                );
            }
            write_secret_file_0600(&path, &db_secret)?;
            tracing::warn!(
                "deployment key: {DEPLOYMENT_KEY_NAME} held an identity the rotation log \
                 supersedes — the committed rotation stands and the file is healed forward \
                 (new nest_actor_id {})",
                hex::encode(&db_public)
            );
            return Ok(());
        }

        // Only write when the DB diverges — a normal restart already has the
        // matching row, so steady-state boots do no write.
        let current = db.get_nest_keypair().await?;
        if current.as_ref().map(|(s, _)| s.as_slice()) != Some(secret.as_slice()) {
            db.set_nest_keypair(secret.as_slice(), &public).await?;
            tracing::warn!(
                "deployment key: restored DB nest_keypair from durable nest_deployment.key \
                 (factory-reset survivor) — channel-binding identity preserved"
            );
        }
        return Ok(());
    }

    // File absent — we are establishing this box's deployment identity for the
    // first time on this disk. A caller-supplied seed wins over whatever random
    // row the migration just seeded: this is the provision-with-identity /
    // total-box-loss-recovery path, where the rebuilt box must re-present the
    // admin's saved `nest_actor_id`.
    if let Some(secret) = seed_override {
        let public = SigningKey::from_bytes(&secret).verifying_key().to_bytes();
        // A re-provision env seed can land beside a DB restored from backup that
        // has since rotated. Installing it would revert the box to an identity
        // every converged client now refuses — the file-shaped twin of the
        // rotate handler's `SupersededAncestor` refusal.
        if superseded_by_rotation(db, &public).await? {
            tracing::error!(
                "{DEPLOYMENT_SEED_ENV} names an identity this box's rotation log supersedes; \
                 ignoring it and adopting the DB's current identity — a revoked deployment \
                 identity never returns (box-recovery.md § The ceremony)"
            );
            return adopt_db_key_into_file(db, &path).await;
        }
        write_secret_file_0600(&path, secret.as_slice())?;
        db.set_nest_keypair(secret.as_slice(), &public).await?;
        tracing::warn!(
            "deployment key: installed caller-supplied {DEPLOYMENT_SEED_ENV} as the box \
             identity (provision-with-identity / total-box-loss recovery) — channel-binding \
             nest_actor_id pinned to the saved seed"
        );
        return Ok(());
    }

    adopt_db_key_into_file(db, &path).await
}

/// Is `public` an identity this box's rotation log has already superseded?
///
/// Reads the ceremony's own append-only log, so the answer is exactly what the
/// committed transaction decided — no second source to drift from.
async fn superseded_by_rotation(db: &CacheDb, public: &[u8; 32]) -> Result<bool> {
    Ok(db.superseded_nest_identities().await?.contains(public))
}

/// A `nest_deployment.key` that exists but is not a 32-byte seed: heal it
/// from the DB `nest_keypair` row if one is present and well-formed, else
/// hard-bail. Deliberately NOT [`adopt_db_key_into_file`] — that function's
/// `None` branch MINTS a fresh identity, which is correct only when there was
/// never a file (a genuinely fresh box, nothing to protect). Here a file DID
/// exist and is now unreadable: minting would silently replace an identity
/// pinned clients may already trust, the exact failure this whole module
/// exists to prevent — so with no DB row to heal from, the only honest answer
/// is the hard bail this replaces (off-box recovery, still required).
async fn heal_malformed_file_or_bail(db: &CacheDb, path: &Path, found_len: usize) -> Result<()> {
    match db.get_nest_keypair().await? {
        Some((secret, _public)) if secret.len() == 32 => {
            write_secret_file_0600(path, &secret)?;
            tracing::warn!(
                "deployment key: {DEPLOYMENT_KEY_NAME} was malformed (expected 32 bytes, got \
                 {found_len}) — healed from the DB nest_keypair row, which stayed authoritative \
                 throughout"
            );
            Ok(())
        }
        Some((secret, _)) => {
            bail!(
                "nest_deployment.key has wrong size (expected 32, got {found_len}) AND the DB \
                 nest_keypair row is also malformed (expected 32, got {}) — nothing to heal \
                 from; this needs off-box recovery",
                secret.len()
            );
        }
        None => {
            bail!(
                "nest_deployment.key has wrong size (expected 32, got {found_len}) and no DB \
                 nest_keypair row exists to heal from — refusing to mint a fresh identity, \
                 which would silently break every pinned client's TOFU channel binding; this \
                 needs off-box recovery"
            );
        }
    }
}

/// Write the DB's `nest_keypair` secret into the durable file (minting one if the
/// row is somehow absent). The tail of the reconcile, shared by the ordinary
/// first-adoption path and the superseded-override refusal above.
async fn adopt_db_key_into_file(db: &CacheDb, path: &Path) -> Result<()> {
    match db.get_nest_keypair().await? {
        Some((secret, _public)) if secret.len() == 32 => {
            write_secret_file_0600(path, &secret)?;
            tracing::info!(
                "deployment key: adopted existing DB nest_keypair into durable nest_deployment.key"
            );
        }
        Some((secret, _)) => {
            bail!(
                "DB nest_keypair secret has wrong size: expected 32, got {}",
                secret.len()
            );
        }
        None => {
            let mut secret = Zeroizing::new([0u8; 32]);
            getrandom::fill(secret.as_mut_slice()).expect("getrandom failed");
            let public = SigningKey::from_bytes(&secret).verifying_key().to_bytes();
            write_secret_file_0600(path, secret.as_slice())?;
            db.set_nest_keypair(secret.as_slice(), &public).await?;
            tracing::warn!(
                "deployment key: generated a new nest_deployment.key (no DB row present)"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn pubkey(db_row: Option<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
        db_row.expect("nest_keypair row present").1
    }

    /// The deployment identity (the channel-binding `nest_actor_id` a client
    /// TOFU-pins) must survive a factory-reset wipe. The wipe deletes `nest.db`,
    /// so a fresh boot's migrations seed a *different* random `nest_keypair`; the
    /// reconcile must restore the preserved key from `nest_deployment.key`.
    #[tokio::test]
    async fn deployment_key_is_stable_across_factory_reset_wipe() {
        let dir = TempDir::new().unwrap();
        let data = dir.path();
        let db_path = data.join("nest.db");

        // Boot 1: fresh DB → migrations seed a random key; reconcile persists it
        // to the durable file.
        let pub1 = {
            let db = CacheDb::open(&db_path).unwrap();
            reconcile_deployment_keypair(&db, data, None).await.unwrap();
            pubkey(db.get_nest_keypair().await.unwrap())
        };
        assert!(
            deployment_key_path(data).exists(),
            "reconcile must persist the deployment key to a durable file"
        );
        // BR-4: the seed file is the nest's irreplaceable identity — owner-only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(deployment_key_path(data))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                mode, 0o600,
                "nest_deployment.key must be 0o600 (owner-only)"
            );
        }

        // Factory-reset wipe: delete nest.db (+ WAL/SHM) exactly as
        // `maybe_run_factory_reset` does — but it never touches nest_deployment.key.
        for sfx in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(data.join(format!("nest.db{sfx}")));
        }
        assert!(
            deployment_key_path(data).exists(),
            "the durable deployment key (the nest's single identity) must survive the wipe"
        );

        // Boot 2: fresh DB → migrations seed a DIFFERENT random key. (Sanity: this
        // is the pre-fix bug — without the reconcile the deployment identity would
        // change across a reset and break every pinned client.)
        let db = CacheDb::open(&db_path).unwrap();
        let migration_pub2 = pubkey(db.get_nest_keypair().await.unwrap());
        assert_ne!(
            pub1, migration_pub2,
            "sanity: each empty DB's migrations seed a fresh random nest_keypair"
        );

        // The reconcile restores the preserved deployment identity.
        reconcile_deployment_keypair(&db, data, None).await.unwrap();
        let pub2 = pubkey(db.get_nest_keypair().await.unwrap());
        assert_eq!(
            pub1, pub2,
            "the deployment identity (channel-binding nest_actor_id) must be STABLE \
             across a factory-reset wipe"
        );
    }

    /// A steady-state restart (DB intact, file present) is a no-op that keeps the
    /// same key — and an existing deployment with no file yet adopts its current
    /// DB key unchanged.
    #[tokio::test]
    async fn reconcile_adopts_existing_db_key_without_changing_it() {
        let dir = TempDir::new().unwrap();
        let data = dir.path();
        let db_path = data.join("nest.db");

        let db = CacheDb::open(&db_path).unwrap();
        let before = pubkey(db.get_nest_keypair().await.unwrap());
        assert!(
            !deployment_key_path(data).exists(),
            "no durable file before the first reconcile"
        );

        // First reconcile (file absent) adopts the existing DB key — unchanged.
        reconcile_deployment_keypair(&db, data, None).await.unwrap();
        assert_eq!(
            before,
            pubkey(db.get_nest_keypair().await.unwrap()),
            "adopting an existing DB key must not rotate it"
        );
        assert!(deployment_key_path(data).exists(), "file now durable");

        // Second reconcile (file present, DB matches) is a no-op.
        reconcile_deployment_keypair(&db, data, None).await.unwrap();
        assert_eq!(before, pubkey(db.get_nest_keypair().await.unwrap()));
    }

    /// The pubkey (channel-binding `nest_actor_id`) a 32-byte seed maps to.
    fn pub_of(seed: &[u8; 32]) -> Vec<u8> {
        SigningKey::from_bytes(seed)
            .verifying_key()
            .to_bytes()
            .to_vec()
    }

    /// Provision-with-identity / total-box-loss recovery (box-recovery.md
    /// § Mechanism): on a **fresh** box (no durable file), a caller-supplied
    /// deployment seed must become the box identity — overriding the *random*
    /// `nest_keypair` the migration just seeded — so the rebuilt box re-presents
    /// the admin's saved `nest_actor_id` and pinned clients reconnect.
    #[tokio::test]
    async fn caller_supplied_seed_is_adopted_on_a_fresh_box() {
        let dir = TempDir::new().unwrap();
        let data = dir.path();
        let db_path = data.join("nest.db");

        let db = CacheDb::open(&db_path).unwrap();
        let migration_pub = pubkey(db.get_nest_keypair().await.unwrap());
        assert!(
            !deployment_key_path(data).exists(),
            "no durable file before the first reconcile"
        );

        let saved_seed = [7u8; 32];
        assert_ne!(
            pub_of(&saved_seed),
            migration_pub,
            "sanity: the saved seed differs from the random migration-seeded row"
        );

        reconcile_deployment_keypair(&db, data, Some(Zeroizing::new(saved_seed)))
            .await
            .unwrap();

        // The DB row + the durable file both now carry the SAVED identity, not
        // the migration's random one.
        assert_eq!(
            pub_of(&saved_seed),
            pubkey(db.get_nest_keypair().await.unwrap()),
            "the caller-supplied seed must beat the migration-seeded random row"
        );
        assert_eq!(
            std::fs::read(deployment_key_path(data)).unwrap(),
            saved_seed.to_vec(),
            "the durable file must hold the saved seed so it persists across reboots"
        );

        // Idempotent: a restart (file now present) with the SAME env seed keeps it.
        reconcile_deployment_keypair(&db, data, Some(Zeroizing::new(saved_seed)))
            .await
            .unwrap();
        assert_eq!(
            pub_of(&saved_seed),
            pubkey(db.get_nest_keypair().await.unwrap())
        );
    }

    /// Once a box has an **established** on-disk identity, a (possibly stale)
    /// provision-time env seed is inert — the deployment seed "rotates only on a
    /// deliberate admin event, never on a reset" (key-material-hierarchy.md). So a
    /// re-provision env landing on a surviving disk must NOT silently rotate the
    /// identity pinned clients already trust.
    #[tokio::test]
    async fn established_identity_wins_over_a_disagreeing_env_seed() {
        let dir = TempDir::new().unwrap();
        let data = dir.path();
        let db_path = data.join("nest.db");

        // Establish identity A from a caller-supplied seed on a fresh box.
        let seed_a = [3u8; 32];
        {
            let db = CacheDb::open(&db_path).unwrap();
            reconcile_deployment_keypair(&db, data, Some(Zeroizing::new(seed_a)))
                .await
                .unwrap();
            assert_eq!(
                pub_of(&seed_a),
                pubkey(db.get_nest_keypair().await.unwrap())
            );
        }

        // Restart with a DIFFERENT env seed B (file from boot 1 survives). The
        // established on-disk identity A must win; B is ignored.
        let seed_b = [9u8; 32];
        assert_ne!(pub_of(&seed_a), pub_of(&seed_b));
        let db = CacheDb::open(&db_path).unwrap();
        reconcile_deployment_keypair(&db, data, Some(Zeroizing::new(seed_b)))
            .await
            .unwrap();
        assert_eq!(
            pub_of(&seed_a),
            pubkey(db.get_nest_keypair().await.unwrap()),
            "an established on-disk identity must win over a disagreeing env seed"
        );
        assert_eq!(
            std::fs::read(deployment_key_path(data)).unwrap(),
            seed_a.to_vec(),
            "the durable file must remain identity A"
        );
    }

    /// The crash the rotation ceremony's post-commit window can produce: the
    /// transaction committed (DB on the successor, log written) but the file
    /// rewrite did not land. "On-disk wins" would silently **un-rotate** the box,
    /// resurrecting the very key the ceremony evicted — so a superseded on-disk
    /// key must lose to the DB and be healed forward.
    #[tokio::test]
    async fn a_superseded_on_disk_key_loses_to_the_committed_rotation() {
        let dir = TempDir::new().unwrap();
        let data = dir.path();
        let db_path = data.join("nest.db");
        let (old, new) = (Zeroizing::new([1u8; 32]), Zeroizing::new([2u8; 32]));

        let db = CacheDb::open(&db_path).unwrap();
        // Boot on the predecessor, so the durable file holds it.
        reconcile_deployment_keypair(&db, data, Some(old.clone()))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(deployment_key_path(data)).unwrap(),
            old.to_vec()
        );

        // Rotate — but simulate the crash by NOT rewriting the file.
        db.rotate_deployment_seed(&old, &new)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read(deployment_key_path(data)).unwrap(),
            old.to_vec(),
            "sanity: the file still holds the superseded key"
        );

        // The next boot heals forward instead of reverting.
        reconcile_deployment_keypair(&db, data, None).await.unwrap();
        assert_eq!(
            pubkey(db.get_nest_keypair().await.unwrap()),
            pub_of(&new),
            "the committed rotation must stand"
        );
        assert_eq!(
            std::fs::read(deployment_key_path(data)).unwrap(),
            new.to_vec(),
            "the durable file must be healed to the successor"
        );
    }

    /// A key the log does NOT supersede keeps the ordinary on-disk-wins rule —
    /// without this, the guard above would be satisfied by simply always
    /// preferring the DB, which would break factory-reset survival.
    #[tokio::test]
    async fn an_unsuperseded_on_disk_key_still_wins_over_the_db() {
        let dir = TempDir::new().unwrap();
        let data = dir.path();
        let db_path = data.join("nest.db");
        let (old, new) = (Zeroizing::new([1u8; 32]), Zeroizing::new([2u8; 32]));

        let db = CacheDb::open(&db_path).unwrap();
        reconcile_deployment_keypair(&db, data, Some(old.clone()))
            .await
            .unwrap();
        db.rotate_deployment_seed(&old, &new)
            .await
            .unwrap()
            .unwrap();
        // Complete the ceremony properly this time.
        write_secret_file_0600(&deployment_key_path(data), new.as_slice()).unwrap();

        // Now scribble a *different, never-superseded* key over the DB row (the
        // factory-reset shape: migrations seeded a fresh random row).
        let stray = [42u8; 32];
        db.set_nest_keypair(&stray, &pub_of(&stray)).await.unwrap();

        reconcile_deployment_keypair(&db, data, None).await.unwrap();
        assert_eq!(
            pubkey(db.get_nest_keypair().await.unwrap()),
            pub_of(&new),
            "the durable file's unsuperseded identity must be restored to the DB"
        );
    }

    /// A re-provision env seed can land beside a DB restored from backup that has
    /// since rotated. Installing it would revert the box to an identity every
    /// converged client refuses.
    #[tokio::test]
    async fn a_superseded_env_seed_is_refused_on_a_fresh_disk() {
        let dir = TempDir::new().unwrap();
        let data = dir.path();
        let db_path = data.join("nest.db");
        let (old, new) = (Zeroizing::new([1u8; 32]), Zeroizing::new([2u8; 32]));

        // A DB that has already rotated old → new, on a disk with no key file
        // (the restore-onto-fresh-disk shape).
        let db = CacheDb::open(&db_path).unwrap();
        db.set_nest_keypair(old.as_slice(), &pub_of(&old))
            .await
            .unwrap();
        db.rotate_deployment_seed(&old, &new)
            .await
            .unwrap()
            .unwrap();
        std::fs::remove_file(deployment_key_path(data)).ok();
        assert!(!deployment_key_path(data).exists());

        // cloud-init still carries the PREDECESSOR seed.
        reconcile_deployment_keypair(&db, data, Some(old.clone()))
            .await
            .unwrap();
        assert_eq!(
            pubkey(db.get_nest_keypair().await.unwrap()),
            pub_of(&new),
            "a superseded env seed must not un-rotate the box"
        );
        assert_eq!(
            std::fs::read(deployment_key_path(data)).unwrap(),
            new.to_vec()
        );
    }

    /// The crash-window this row exists to close: a `nest_deployment.key`
    /// truncated to 0 bytes (the shape a crash mid-rewrite left under the
    /// pre-fix truncate-then-write) must NOT bail the boot when the DB row
    /// that would heal it sits right there. Reconcile restores the file
    /// byte-identically from the still-authoritative DB row, and the
    /// identity is unchanged.
    #[tokio::test]
    async fn a_malformed_file_heals_from_the_db_row() {
        let dir = TempDir::new().unwrap();
        let data = dir.path();
        let db_path = data.join("nest.db");

        let db = CacheDb::open(&db_path).unwrap();
        reconcile_deployment_keypair(&db, data, None).await.unwrap();
        let established = pubkey(db.get_nest_keypair().await.unwrap());
        let good_bytes = std::fs::read(deployment_key_path(data)).unwrap();
        assert_eq!(good_bytes.len(), 32, "sanity: a real seed is 32 bytes");

        // Simulate the crash-mid-rewrite shape directly: truncate the durable
        // file to 0 bytes, leaving the DB row (still authoritative) untouched.
        std::fs::write(deployment_key_path(data), []).unwrap();
        assert_eq!(
            std::fs::metadata(deployment_key_path(data)).unwrap().len(),
            0
        );

        reconcile_deployment_keypair(&db, data, None).await.unwrap();

        assert_eq!(
            std::fs::read(deployment_key_path(data)).unwrap(),
            good_bytes,
            "the file must heal byte-identically from the DB row"
        );
        assert_eq!(
            pubkey(db.get_nest_keypair().await.unwrap()),
            established,
            "healing must not change the deployment identity"
        );
    }

    /// A malformed file with NO DB row to heal from must still hard-bail —
    /// there is nothing authoritative to recover from, and minting a fresh
    /// identity would silently break every pinned client's TOFU channel
    /// binding, which is exactly the failure this module exists to prevent.
    #[tokio::test]
    async fn a_malformed_file_with_no_db_row_still_bails() {
        let dir = TempDir::new().unwrap();
        let data = dir.path();
        let db_path = data.join("nest.db");

        let db = CacheDb::open(&db_path).unwrap();
        // Clear the row migrations seed, so there is truly nothing to heal
        // from (the shape `adopt_db_key_into_file`'s own doc comment calls
        // "unreachable in practice" via the normal boot path — reached here
        // directly to exercise the hard-bail branch).
        db.execute_batch("DELETE FROM nest_keypair").await.unwrap();
        assert_eq!(db.get_nest_keypair().await.unwrap(), None);

        std::fs::write(deployment_key_path(data), [0u8; 5]).unwrap();

        let err = reconcile_deployment_keypair(&db, data, None)
            .await
            .expect_err("a malformed file with no DB row must bail, not mint a fresh identity");
        let msg = err.to_string();
        assert!(
            msg.contains("nothing to heal") || msg.contains("refusing to mint"),
            "the error must say why it refused to proceed, got: {msg}"
        );
    }

    /// The rotation-heal path forces a real rewrite through the shared
    /// `write_secret_file_0600` (its own atomicity/crash-safety properties
    /// are unit-tested directly in `fauna_core::secret_file`) — this only
    /// proves THIS crate's call path actually replaces the file via a new
    /// inode rather than an in-place truncate.
    #[tokio::test]
    async fn rotation_heal_replaces_via_rename_not_in_place_truncate() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;

            let dir = TempDir::new().unwrap();
            let data = dir.path();
            let db_path = data.join("nest.db");

            let db = CacheDb::open(&db_path).unwrap();
            reconcile_deployment_keypair(&db, data, None).await.unwrap();
            let path = deployment_key_path(data);
            let ino_before = std::fs::metadata(&path).unwrap().ino();

            write_secret_file_0600(&path, &[9u8; 32]).unwrap();

            let ino_after = std::fs::metadata(&path).unwrap().ino();
            assert_ne!(
                ino_before, ino_after,
                "a rewrite must replace the file via rename(2) onto a fresh inode, never \
                 truncate-and-rewrite the live path in place — otherwise a crash between the \
                 truncate and the write can leave the file short"
            );
        }
    }

    #[test]
    fn decode_deployment_seed_round_trips_and_rejects_malformed() {
        let seed = [0xABu8; 32];
        let hexed = hex::encode(seed);
        assert_eq!(decode_deployment_seed(&hexed).as_deref(), Some(&seed));
        // Surrounding whitespace (a stray newline in cloud-init) is tolerated.
        assert_eq!(
            decode_deployment_seed(&format!("  {hexed}\n")).as_deref(),
            Some(&seed)
        );

        // Wrong length, non-hex, and empty all reject (None → mint own identity).
        assert_eq!(decode_deployment_seed(&hex::encode([0u8; 31])), None);
        assert_eq!(decode_deployment_seed(&hex::encode([0u8; 33])), None);
        assert_eq!(decode_deployment_seed("not-hex-at-all"), None);
        assert_eq!(decode_deployment_seed(""), None);
    }
}
