//! The materializer's **capability-holder identity** — the nest's second
//! in-process holder, on the web-serve holder's template
//! ([`crate::web_content::holder`]; `encryption-at-rest.md` § Capability
//! tiering → *The materialization grant*, part (b)).
//!
//! Its own seed file in the data dir (`materializer_holder.key`, 0600, NOT
//! factory-reset-preserved: a reset wipes the grants anyway, so the apps
//! re-mint to the fresh key), an X25519 and ML-KEM pair derived under its own
//! context string, a self-approved `bridge_service_users` row (role
//! `content-processor`, `bridge_id = "materializer"`, `in_process`) and a
//! [`fauna_capability_holder::Registry`] whose fetch seam is the direct
//! `capability_grants` read. An admin's revocation of the row is respected —
//! never resurrected at boot — and then the materializer holds nothing and
//! every member's views are deleted (an admin can always stop a component of
//! their box; the consequence is honest).
//!
//! One holder per purpose: this is not the web-serve holder (its roster row
//! reads "keeps your mail searchable", never "serves paywalled posts") and not
//! a third-party principal — it is the box itself, which class 4 names as the
//! holder.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use fauna_capability_holder::Registry;
use fauna_mls::wrapped_blob::{
    MLKEM768_ENCAPS_KEY_LEN, derive_bridge_service_user_mlkem768, derive_x25519_keypair_from_ikm,
};
use zeroize::Zeroizing;

use crate::db::CacheDb;
use crate::db::bridge_service_users::{BridgeRole, BridgeStatus};

/// Filename of the materializer holder seed in the nest data dir.
pub const HOLDER_KEY_NAME: &str = "materializer_holder.key";

/// The enrollment row's `bridge_id` — the id every member's mint picker keys
/// on — the same spelling as `fauna_client_bridges::MATERIALIZER_HOLDER_ID`,
/// the shared constant the apps read (the nest does not link that client
/// crate, so the spelling is repeated here and pinned by the conformance test
/// `conformance_materialization_grant`, which lists the roster as a user).
pub const MATERIALIZER_BRIDGE_ID: &str = "materializer";

/// Domain separator for the holder's X25519 keypair — its own, distinct from
/// the web-serve holder's and every other derivation off seed material.
const HOLDER_X25519_DERIVE_CONTEXT: &str = "fauna.materializer-holder.x25519.v1";

/// The materializer's holder identity and its live grant registry.
pub struct MaterializerHolder {
    ed25519_pubkey: [u8; 32],
    /// The enrolled X25519 public half grants are sealed to — the key the
    /// mint door compares a `materialize` grant's holder against.
    pub x25519_pubkey: [u8; 32],
    /// The published ML-KEM-768 encapsulation key (X-Wing hybrid mints).
    pub mlkem_ek: [u8; MLKEM768_ENCAPS_KEY_LEN],
    /// The live holder registry (fetch = direct `capability_grants` read).
    pub registry: Registry,
}

/// Path to the holder seed for a given data dir.
pub fn holder_key_path(data_dir: &Path) -> PathBuf {
    data_dir.join(HOLDER_KEY_NAME)
}

fn load_or_create_seed(path: &Path) -> Result<Zeroizing<[u8; 32]>> {
    if path.exists() {
        let bytes = std::fs::read(path)
            .with_context(|| format!("read materializer holder seed {}", path.display()))?;
        let seed: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("holder seed must be 32 bytes, got {}", bytes.len()))?;
        return Ok(Zeroizing::new(seed));
    }
    let mut seed = Zeroizing::new([0u8; 32]);
    getrandom::fill(seed.as_mut()).expect("getrandom failed");
    // The data dir is not guaranteed to exist yet (a segment manager creates
    // its dirs lazily, on the first append), so the first boot creates it.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create the data dir {}", parent.display()))?;
    }
    crate::deployment_key::write_secret_file_0600(path, seed.as_slice())
        .with_context(|| format!("write materializer holder seed {}", path.display()))?;
    tracing::info!(target: "materializer", path = %path.display(), "generated materializer holder seed");
    Ok(seed)
}

impl MaterializerHolder {
    /// Load (or mint) the holder identity, self-approve its enrollment row and
    /// build its grant registry. `Ok(None)` when an admin revoked the row: the
    /// holder stays dark and the caller deletes every view.
    pub async fn init(data_dir: &Path, db: Arc<CacheDb>) -> Result<Option<Self>> {
        let seed = load_or_create_seed(&holder_key_path(data_dir))?;
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
        let ed25519_pubkey: [u8; 32] = signing_key.verifying_key().to_bytes();

        let x25519_ikm = blake3::derive_key(HOLDER_X25519_DERIVE_CONTEXT, seed.as_ref());
        let (x25519_secret, x25519_pubkey) = derive_x25519_keypair_from_ikm(&x25519_ikm);
        let x25519_secret = Zeroizing::new(x25519_secret);
        // The bridge-service-user ML-KEM derivation carries its own context
        // string and is keyed on this holder's own seed, so the pair is this
        // holder's alone (the web-serve holder's seed is a different file).
        let (mlkem_dk, mlkem_ek) = derive_bridge_service_user_mlkem768(seed.as_ref());

        match db.lookup_bridge_service_user(&ed25519_pubkey).await? {
            Some(row) if row.status == BridgeStatus::Revoked => {
                tracing::warn!(
                    target: "materializer",
                    "materializer holder enrollment is REVOKED by admin; no view is kept"
                );
                return Ok(None);
            }
            Some(row) if row.status == BridgeStatus::Pending => {
                db.approve_bridge_service_user(&ed25519_pubkey, None)
                    .await?;
                debug_assert_eq!(row.role, BridgeRole::ContentProcessor);
            }
            Some(_approved) => {}
            None => {
                db.create_pending_bridge_service_user(
                    &ed25519_pubkey,
                    BridgeRole::ContentProcessor,
                    MATERIALIZER_BRIDGE_ID,
                )
                .await?;
                if !db
                    .approve_bridge_service_user(&ed25519_pubkey, None)
                    .await?
                {
                    bail!("self-approve of fresh materializer holder enrollment did not apply");
                }
            }
        }
        // In-process: never a seal target for content that must rest
        // nest-opaque, and never dials in (the web-serve holder's rule — every
        // in-process holder marks its own row).
        db.mark_bridge_service_user_in_process(&ed25519_pubkey)
            .await?;
        db.upsert_bridge_x25519(&ed25519_pubkey, &x25519_pubkey)
            .await
            .context("bind materializer holder x25519")?;
        db.upsert_bridge_mlkem_ek(&ed25519_pubkey, &mlkem_ek)
            .await
            .context("publish materializer holder ML-KEM ek")?;

        let fetch_db = db.clone();
        let registry = Registry::new(
            *x25519_secret,
            Some(mlkem_dk),
            Box::new(move || {
                let db = fetch_db.clone();
                let holder = x25519_pubkey;
                Box::pin(async move {
                    db.fetch_capability_grants_for_holder(&holder, crate::db::now_epoch_secs())
                        .await
                        .map_err(|e| e.to_string())
                })
            }),
        );
        if let Err(e) = registry.refresh().await {
            tracing::warn!(target: "materializer", %e, "initial capability-grant fetch failed");
        }
        Ok(Some(Self {
            ed25519_pubkey,
            x25519_pubkey,
            mlkem_ek,
            registry,
        }))
    }

    /// The holder's Ed25519 public identity (the enrollment-row key).
    pub fn ed25519_pubkey(&self) -> [u8; 32] {
        self.ed25519_pubkey
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn init_enrolls_an_approved_in_process_content_processor_with_a_0600_seed() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let holder = MaterializerHolder::init(dir.path(), db.clone())
            .await
            .unwrap()
            .expect("holder must init");
        let row = db
            .lookup_bridge_service_user(&holder.ed25519_pubkey())
            .await
            .unwrap()
            .expect("enrollment row");
        assert_eq!(row.status, BridgeStatus::Approved);
        assert_eq!(row.role, BridgeRole::ContentProcessor);
        assert_eq!(row.bridge_id, MATERIALIZER_BRIDGE_ID);
        assert_eq!(row.x25519_pubkey, Some(holder.x25519_pubkey));
        assert!(row.in_process);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(holder_key_path(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the seed rests owner-only");
        }
        let again = MaterializerHolder::init(dir.path(), db.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            again.x25519_pubkey, holder.x25519_pubkey,
            "same seed, same key"
        );
    }

    #[tokio::test]
    async fn its_key_is_not_the_web_serve_holders() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let ours = MaterializerHolder::init(dir.path(), db.clone())
            .await
            .unwrap()
            .unwrap();
        let web = crate::web_content::holder::WebServeHolder::init(dir.path(), db.clone())
            .await
            .unwrap()
            .unwrap();
        assert_ne!(ours.x25519_pubkey, web.x25519_pubkey);
        assert_ne!(ours.ed25519_pubkey(), web.ed25519_pubkey());
    }

    #[tokio::test]
    async fn an_admin_revocation_is_respected_not_resurrected() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let holder = MaterializerHolder::init(dir.path(), db.clone())
            .await
            .unwrap()
            .unwrap();
        db.revoke_bridge_service_user(&holder.ed25519_pubkey())
            .await
            .unwrap();
        assert!(
            MaterializerHolder::init(dir.path(), db.clone())
                .await
                .unwrap()
                .is_none()
        );
        let row = db
            .lookup_bridge_service_user(&holder.ed25519_pubkey())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, BridgeStatus::Revoked);
    }
}
