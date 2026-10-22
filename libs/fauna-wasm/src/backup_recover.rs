//! The web composition of the **lived-in recovery** — the browser twin of
//! `fauna_sync_engine::recovery::recover_from_destination`, and like it a
//! wiring of seams over shared Rust that decides nothing.
//!
//! Owner: `docs/goal/architecture/segment-backup-protocol.md` § Client-device
//! custodian (pull) → *Restore* → *Recovery into the lived-in nest that
//! regressed*, part (1) — "web-capable like the pull-back": the leg runs in the
//! app over the owner's own connections and touches no custodian store, so a
//! browser runs it as a native app does. The gesture and its result view:
//! `docs/goal/ui/backups.md` § Audit-alert surface, the fifth reason.
//!
//! The order and the result are the shared driver's
//! ([`fauna_client_backup::recover::run_recovery`]), the leg the shared
//! [`fauna_client_backup::recover_leg::RecoveryDelivery`]. Web supplies only
//! what a browser has:
//!
//! * the owner's own authenticated session to the **destination** — the
//!   enroll path's two-step connect (`crate::pairing::make_backup_connect`'s
//!   `resolve_and_authorize_destination_inner`), held to the identity the
//!   owner enrolled, the refusal `fauna_client_backup::trust::connect_destination`
//!   makes;
//! * the destination's open, hash-verified byte routes
//!   ([`fauna_core::file_download::WasmPublicChunkFetcher`], the audit's
//!   inclusion fetcher);
//! * the source's owner-session byte plane ([`PageBytePlane`], the page's own
//!   bearer over the chunk-store routes);
//! * this device's `localStorage` audit store, whose notice record names the
//!   sets to recover and which the leg's duty clears.
//!
//! Web is never a custodian, so it has no store end: only a nest destination's
//! banner carries the gesture here
//! ([`fauna_client_backup::recover::recover_offered`] with no device id).

#![cfg(target_arch = "wasm32")]

use fauna_client_backup::recover::{regressed_sets, result_lines, run_recovery};
use fauna_client_backup::recover_leg::{
    AuditStoreRegressionRecord, BlobPushSink, RecoveryDelivery, RecoveryDestination, RecoverySource,
};
use fauna_core::crypto::NestBackupKey;
use fauna_core::data::{ContentHash, DestinationKind};
use fauna_core::identity::ActorKeypair;
use fauna_core::localized::LocalizedText;
use fauna_rpc_wasm::{TokenWsRpcClient, WsRpcClient as InnerClient};
use serde::Serialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use crate::rpc::{WsRpcClient, err_to_js, to_js};

fauna_client_backup::impl_backup_nest_seam!(struct RecoveryDestinationNest<TokenWsRpcClient>);

/// The source's **owner-session byte plane** on web: the chunk-store write
/// routes (`POST /api/v1/chunks`, `/api/v1/manifests`) under the page's own
/// session bearer — the role the sync engine's pooled transfer client plays
/// natively. `fauna_rpc_wasm::post_octets_keyed` owns the request shape (the
/// Media page's content-keyed uploads ride the same helper).
///
/// No dedup check: the browser has no `POST /chunks/check` leg, and the store
/// is content-addressed with a deterministic seal, so re-putting a chunk the
/// nest already holds is idempotent — the check is an economy, never a
/// correctness question.
pub(crate) struct PageBytePlane {
    pub(crate) nest: InnerClient,
}

impl PageBytePlane {
    /// The nest files every body under its own hash of it and answers
    /// `{"hash": "<64 hex>"}`; a body filed under any other key is a failed
    /// landing, never a success.
    fn verify_filed_under(reply: &str, expected: &ContentHash, what: &str) -> anyhow::Result<()> {
        #[derive(serde::Deserialize)]
        struct Filed {
            hash: String,
        }
        let filed: Filed = serde_json::from_str(reply)
            .map_err(|e| anyhow::anyhow!("{what} upload: unreadable reply: {e}"))?;
        let want = hex::encode(expected.digest());
        anyhow::ensure!(
            filed.hash.eq_ignore_ascii_case(&want),
            "{what} upload: the nest filed the body under {}, not {want}",
            filed.hash
        );
        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl BlobPushSink for PageBytePlane {
    async fn missing_chunks(&self, store_keys: &[ContentHash]) -> anyhow::Result<Vec<ContentHash>> {
        Ok(store_keys.to_vec())
    }

    async fn put_chunk(&self, store_key: &ContentHash, body: &[u8]) -> anyhow::Result<()> {
        let reply = fauna_rpc_wasm::post_octets_keyed(
            &self.nest,
            fauna_rpc_wasm::CHUNKS_UPLOAD_PATH,
            Some(&hex::encode(store_key.digest())),
            body,
        )
        .await
        .map_err(|e| anyhow::anyhow!("chunk upload: {e}"))?;
        Self::verify_filed_under(&reply, store_key, "chunk")
    }

    async fn put_manifest(&self, manifest_bytes: &[u8]) -> anyhow::Result<()> {
        let reply = fauna_rpc_wasm::post_octets_keyed(
            &self.nest,
            fauna_rpc_wasm::MANIFESTS_UPLOAD_PATH,
            None,
            manifest_bytes,
        )
        .await
        .map_err(|e| anyhow::anyhow!("manifest upload: {e}"))?;
        let hash = ContentHash::from_digest_raw(*blake3::hash(manifest_bytes).as_bytes());
        Self::verify_filed_under(&reply, &hash, "manifest")
    }
}

/// The `backup-audit-recover-result` view's content, for JS: the shared
/// verdict bit and the shared result lines, verdict first — each a
/// `LocalizedText` the SPA resolves with `resolveLocalizedNested` (a line's set
/// name and remedy are themselves keys), the wasm twin of the ffi
/// `FfiRecoveryResult`.
#[derive(Serialize)]
struct WasmRecoveryResult {
    /// [`fauna_client_backup::recover::RecoveryOutcome::is_whole`] — the one
    /// bit the page may render as "recovered".
    whole: bool,
    lines: Vec<LocalizedText>,
}

#[wasm_bindgen]
impl WsRpcClient {
    /// Run the recovery from `destination_id` into the owner's nest — the
    /// confirm of `backup-audit-recover-button` (`ui/backups.md` § Audit-alert
    /// surface, the fifth reason). Resolves to `{ whole, lines }`; after it
    /// settles the page runs `backupAuditRunPass` again, so the cleared notice
    /// is read from the store, never assumed.
    ///
    /// `destination_id` is the row whose `recover_offered` was true, resolved
    /// against the client's **own pinned** destination list as the audit pass
    /// resolves it. Rejects with a message for a run that never reached the
    /// shared driver or that the driver stopped (a destination not on the list
    /// or not a nest, one that will not connect or proves another identity, a
    /// transport fault mid-run) — nothing the owner can lose: a re-run resumes
    /// by content address and the verb recovers nothing twice.
    #[wasm_bindgen(js_name = backupRecoverFromDestination)]
    pub fn backup_recover_from_destination(
        &self,
        secret_hex: String,
        destination_id: String,
    ) -> js_sys::Promise {
        let client = self.inner_client();
        future_to_promise(async move {
            let result = recover_from_destination(client, secret_hex, destination_id).await?;
            to_js(&result)
        })
    }
}

async fn recover_from_destination(
    source: InnerClient,
    secret_hex: String,
    destination_id: String,
) -> Result<WasmRecoveryResult, JsValue> {
    let secret = fauna_core::hex32::decode(&secret_hex).map_err(err_to_js)?;
    let keypair = ActorKeypair::from_secret(secret);
    let actor_hex = keypair.actor_id_hex();
    let scope_id = keypair.actor_id().0;

    let source_nest = crate::rpc::bound_nest_id(&source).await?.0;
    let state = fauna_client_config::load_backup_state(
        &*crate::account_runtime::backup_seam(),
        source_nest,
    )
    .await
    .map_err(err_to_js)?;
    let destination = state
        .backup
        .destinations
        .iter()
        .find(|d| d.destination_id == destination_id)
        .ok_or_else(|| JsValue::from_str(&format!("no backup destination {destination_id}")))?;
    if !matches!(destination.kind_view(), DestinationKind::Nest { .. }) {
        return Err(JsValue::from_str(&format!(
            "{destination_id} is not a nest, so there is no copy to recover from over the network"
        )));
    }

    let audit = crate::backup_audit::LocalStorageAuditStateStore::for_actor(actor_hex.clone());
    let snapshot = fauna_client_backup::audit::AuditStateStore::load(&audit)
        .map_err(|e| JsValue::from_str(&format!("reading this device's backup checks: {e}")))?;
    let sets = snapshot
        .records
        .iter()
        .find(|r| r.state.destination_id == destination.destination_id)
        .map(regressed_sets)
        .unwrap_or_default();
    let outcome = if sets.is_empty() {
        // Nothing stands: the notice cleared since it was painted.
        Default::default()
    } else {
        let url = destination.destination_nest_url.trim_end_matches('/');
        let (proven, _domain, dest) =
            crate::rpc::resolve_and_authorize_destination_inner(&secret_hex, url).await?;
        if proven != destination.destination_actor_pubkey {
            return Err(JsValue::from_str(&format!(
                "{url} proved identity {}, not the destination enrolled as {} — refusing to \
                 recover from it",
                fauna_core::hex32::encode(&proven),
                fauna_core::hex32::encode(&destination.destination_actor_pubkey),
            )));
        }
        let device_hex = crate::rpc::this_browsers_device_id(&actor_hex).map_err(err_to_js)?;

        let seam = RecoveryDestinationNest {
            client: fauna_client_backup::BackupClient::new(dest.clone()),
        };
        let fetcher = fauna_core::file_download::WasmPublicChunkFetcher::new(url);
        let dest_records = fauna_client_sync::SyncClient::new(&dest);
        let sink = PageBytePlane {
            nest: source.clone(),
        };
        let source_records = fauna_client_sync::SyncClient::new(&source);
        let regression = AuditStoreRegressionRecord {
            store: &audit,
            destination_id: &destination.destination_id,
            scope_id,
        };
        let leg = RecoveryDelivery::new(
            RecoveryDestination {
                seam: &seam,
                bytes: &fetcher,
                records: &dest_records,
                device_id: device_hex.clone(),
            },
            RecoverySource {
                bytes: &sink,
                nest: &source,
                records: &source_records,
                device_id: device_hex,
            },
            scope_id,
            &NestBackupKey::derive(&secret),
            &regression,
        );
        run_recovery(&leg, &sets).await.map_err(err_to_js)?
    };
    Ok(WasmRecoveryResult {
        whole: outcome.is_whole(),
        lines: result_lines(&outcome),
    })
}
