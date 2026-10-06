//! The nest web-serve component's **capability-holder identity** — the Pillar-2
//! web-paywall holder (`docs/goal/behavior/monetization.md` § Pillar 2;
//! `docs/goal/architecture/encryption-at-rest.md` § Readable classes item 2).
//!
//! The web-serve component enrolls its **own service-user identity** (its own
//! X25519 keypair via the standard bridge service-user enrollment rows,
//! `content-processor` role family) so a creator's client can mint it a
//! standing scoped capability grant — exactly the paywall tier's `period_key`
//! and/or `web`-folder keys. Grants are sealed to this identity, never to the
//! nest's storage layer: the unwrapped keys exist only in the in-memory
//! [`fauna_capability_holder::Registry`] under the standard holder contract
//! (transient-fetch-error keeps cached; authoritative absent verdict zeroizes;
//! revoke bites at use).
//!
//! **Enrollment is in-process artifact wiring, not an admin choice.** An
//! external bridge lands `pending` and waits for explicit admin approval; the
//! web-serve component IS the nest binary, so its enrollment is self-approved
//! at boot — the same trust class as the MDA's loopback auto-approve (the
//! supervisor wiring already decided this component runs). The security
//! boundary is unchanged: the **off-box mint authority** — nothing is readable
//! until a creator's client mints a grant, and the enrollment row makes the
//! holder visible on the standard capability audit/revoke surface. An admin
//! revocation of the row is respected (never resurrected at boot): the paywall
//! holder then fetches nothing and paywalled serving darkens to teasers.
//!
//! The identity seed lives in `web_serve_holder.key` (a raw 32-byte seed in the
//! data dir, 0600 — the `nest_deployment.key` pattern). Unlike the deployment
//! key it is NOT factory-reset-preserved: a reset wipes `capability_grants`
//! anyway, so clients re-mint to the fresh holder pubkey after re-claim.

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

/// Filename of the web-serve holder seed in the nest data dir.
const HOLDER_KEY_NAME: &str = "web_serve_holder.key";

/// The enrollment row's `bridge_id` — the stable name `fetch_bridge_pubkey`
/// discovery keys on (role `content-processor` + this id).
pub const WEB_SERVE_BRIDGE_ID: &str = "web-serve";

/// Domain separator for deriving the holder's X25519 keypair from its Ed25519
/// seed. Distinct from every other derivation off seed material (mirrors
/// `BRIDGE_SERVICE_USER_MLKEM_DERIVE_CONTEXT`'s separation rationale).
const HOLDER_X25519_DERIVE_CONTEXT: &str = "fauna.web-serve-holder.x25519.v1";

/// The web-serve component's holder identity + live grant registry.
pub struct WebServeHolder {
    /// The holder's Ed25519 identity (enrollment-row key; also the
    /// capability-URL token-signing key — § Pillar 2 gate).
    signing_key: ed25519_dalek::SigningKey,
    /// The enrolled X25519 public half grants are sealed to.
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

/// Load the 32-byte holder seed, generating one on first boot (0600, the
/// `deployment_key` file pattern).
fn load_or_create_seed(path: &Path) -> Result<Zeroizing<[u8; 32]>> {
    if path.exists() {
        let bytes = std::fs::read(path)
            .with_context(|| format!("read web-serve holder seed {}", path.display()))?;
        let seed: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("holder seed must be 32 bytes, got {}", bytes.len()))?;
        return Ok(Zeroizing::new(seed));
    }
    let mut seed = Zeroizing::new([0u8; 32]);
    getrandom::fill(seed.as_mut()).expect("getrandom failed");
    crate::deployment_key::write_secret_file_0600(path, seed.as_slice())
        .with_context(|| format!("write web-serve holder seed {}", path.display()))?;
    tracing::info!(target: "web_serve_holder", path = %path.display(), "generated web-serve holder seed");
    Ok(seed)
}

impl WebServeHolder {
    /// Load (or mint) the holder identity, self-approve its enrollment rows,
    /// and build its grant registry over a direct `capability_grants` fetch.
    ///
    /// Returns `Ok(None)` when the enrollment row exists but is **revoked** —
    /// an admin turned this holder off; respect it (paywalled serving darkens
    /// to teasers) rather than resurrecting the row.
    pub async fn init(data_dir: &Path, db: Arc<CacheDb>) -> Result<Option<Arc<Self>>> {
        let seed = load_or_create_seed(&holder_key_path(data_dir))?;
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
        let ed25519_pubkey: [u8; 32] = signing_key.verifying_key().to_bytes();

        // Domain-separated X25519 (grant-unseal) + ML-KEM (hybrid) halves.
        let x25519_ikm = blake3::derive_key(HOLDER_X25519_DERIVE_CONTEXT, seed.as_ref());
        let (x25519_secret, x25519_pubkey) = derive_x25519_keypair_from_ikm(&x25519_ikm);
        let x25519_secret = Zeroizing::new(x25519_secret);
        let (mlkem_dk, mlkem_ek) = derive_bridge_service_user_mlkem768(seed.as_ref());

        // ── Enrollment rows (idempotent, self-approved — module docs) ──
        match db.lookup_bridge_service_user(&ed25519_pubkey).await? {
            Some(row) if row.status == BridgeStatus::Revoked => {
                tracing::warn!(
                    target: "web_serve_holder",
                    "web-serve holder enrollment is REVOKED by admin; paywalled serving stays dark"
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
                    WEB_SERVE_BRIDGE_ID,
                )
                .await?;
                if !db
                    .approve_bridge_service_user(&ed25519_pubkey, None)
                    .await?
                {
                    bail!("self-approve of fresh web-serve holder enrollment did not apply");
                }
            }
        }
        // This holder is IN-PROCESS: its X25519 secret derives from a seed in
        // the nest's own data dir, so it must never be resolved as a seal
        // target for content that must rest nest-opaque (the spam-baseline
        // aggregation holder — `mail-spam.md` § Encrypted-mode interaction:
        // "never nest in-process"), and it never dials in over WS, so pokes
        // addressed to it are always dropped. Marked on every boot — keyed on
        // the actual pubkey — because enrollment writes the row with
        // `in_process = 0` and this self-mark is the only setter. Every future in-process
        // holder must mark its own row the same way.
        db.mark_bridge_service_user_in_process(&ed25519_pubkey)
            .await?;
        // Set-once bindings; idempotent for the same derived keys.
        db.upsert_bridge_x25519(&ed25519_pubkey, &x25519_pubkey)
            .await
            .context("bind web-serve holder x25519")?;
        db.upsert_bridge_mlkem_ek(&ed25519_pubkey, &mlkem_ek)
            .await
            .context("publish web-serve holder ML-KEM ek")?;

        // ── Registry over a direct DB fetch (the in-process analogue of the
        // WS-RPC `fauna.capabilities.fetch`; same holder-pubkey scoping +
        // expiry filter as `fetch_grants_handler`). ──
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
        // First fetch: best-effort — an empty/failed read just means "no
        // grants yet"; the mint/renew/revoke handlers poke refresh live.
        if let Err(e) = registry.refresh().await {
            tracing::warn!(target: "web_serve_holder", %e, "initial capability-grant fetch failed");
        }

        Ok(Some(Arc::new(Self {
            signing_key,
            x25519_pubkey,
            mlkem_ek,
            registry,
        })))
    }

    /// The holder's Ed25519 public identity (the enrollment-row key).
    pub fn ed25519_pubkey(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    /// The token-signing key (capability-URL mint — § Pillar 2 gate).
    pub fn signing_key(&self) -> &ed25519_dalek::SigningKey {
        &self.signing_key
    }

    /// The holder identity as an [`ActorKeypair`] — the shape
    /// `encoding::sign_and_pack` (the capability-URL token mint) takes.
    pub fn actor_keypair(&self) -> fauna_core::identity::ActorKeypair {
        fauna_core::identity::ActorKeypair::from_secret(self.signing_key.to_bytes())
    }
}

#[cfg(test)]
mod tests {
    use fauna_mls::wrapped_blob::{GrantWindow, ScopeTuple, build_grant_blob};

    use super::*;

    async fn init_holder(dir: &Path, db: &Arc<CacheDb>) -> Option<Arc<WebServeHolder>> {
        WebServeHolder::init(dir, db.clone()).await.unwrap()
    }

    fn mint_post_grant_blob(
        owner: &[u8; 32],
        grant_id: &[u8; 16],
        holder: &WebServeHolder,
        tier: &str,
        period_key: &[u8; 32],
    ) -> Vec<u8> {
        build_grant_blob(
            owner,
            grant_id,
            &holder.x25519_pubkey,
            Some(&holder.mlkem_ek),
            GrantWindow(0, u64::MAX),
            &[(
                ScopeTuple {
                    class: ScopeTuple::CLASS_CONTENT_READ.to_string(),
                    kind: Some(ScopeTuple::KIND_POST.to_string()),
                    tier: Some(tier.to_string()),
                    set: None,
                    factor: None,
                },
                Some(period_key.to_vec()),
            )],
        )
        .unwrap()
        .to_canonical_bytes()
        .unwrap()
    }

    #[tokio::test]
    async fn init_is_idempotent_and_enrolls_approved_content_processor() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(CacheDb::open_in_memory().unwrap());

        let holder = init_holder(dir.path(), &db)
            .await
            .expect("holder must init");
        let row = db
            .lookup_bridge_service_user(&holder.ed25519_pubkey())
            .await
            .unwrap()
            .expect("enrollment row must exist");
        assert_eq!(row.status, BridgeStatus::Approved);
        assert_eq!(row.role, BridgeRole::ContentProcessor);
        assert_eq!(row.bridge_id, WEB_SERVE_BRIDGE_ID);
        assert_eq!(row.x25519_pubkey, Some(holder.x25519_pubkey));
        assert!(
            row.in_process,
            "the self-enrollment must mark the row in-process — the spam-baseline \
             aggregation resolver excludes it on this marker"
        );

        // Second boot off the same seed file: same identity, no duplicate row,
        // set-once key bindings accept the byte-identical re-attestation.
        let again = init_holder(dir.path(), &db)
            .await
            .expect("re-init must succeed");
        assert_eq!(again.ed25519_pubkey(), holder.ed25519_pubkey());
        assert_eq!(again.x25519_pubkey, holder.x25519_pubkey);
    }

    #[tokio::test]
    async fn admin_revocation_is_respected_not_resurrected() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let holder = init_holder(dir.path(), &db)
            .await
            .expect("holder must init");
        db.revoke_bridge_service_user(&holder.ed25519_pubkey())
            .await
            .unwrap();
        assert!(
            init_holder(dir.path(), &db).await.is_none(),
            "a revoked enrollment must keep the holder dark, not re-approve it"
        );
        let row = db
            .lookup_bridge_service_user(&holder.ed25519_pubkey())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, BridgeStatus::Revoked);
    }

    #[tokio::test]
    async fn grant_lifecycle_mint_fetch_revoke_darkens() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let holder = init_holder(dir.path(), &db)
            .await
            .expect("holder must init");

        let owner = [9u8; 32];
        let grant_id = [3u8; 16];
        let period_key = [77u8; 32];
        let blob = mint_post_grant_blob(&owner, &grant_id, &holder, "gold", &period_key);

        // Mint (the handler's storage write) → refresh → the key is wielded.
        db.put_capability_grant(&owner, &grant_id, &holder.x25519_pubkey, i64::MAX, &blob)
            .await
            .unwrap();
        holder.registry.refresh().await.unwrap();
        let set = holder.registry.current();
        assert_eq!(
            set.key_for(&owner, "content.read", "post", Some("gold"), 1),
            Some(&period_key[..])
        );

        // Revoke (the handler's delete) → refresh → authoritative-absent darkens.
        db.delete_capability_grant(&owner, &grant_id).await.unwrap();
        holder.registry.refresh().await.unwrap();
        assert!(
            holder
                .registry
                .current()
                .key_for(&owner, "content.read", "post", Some("gold"), 1)
                .is_none(),
            "revoked grant must go dark on the next fetch"
        );
    }
}
