//! The web composition of the **nest-held pull-back** — the browser twin of
//! `fauna_sync_engine::reseed_pull::pull_back_from_destination`, and like it a
//! wiring of seams over shared Rust that decides nothing.
//!
//! Owner: `docs/goal/architecture/segment-backup-protocol.md` § Client-device
//! custodian (pull) → *Restore* → *The nest-held pull-back*; the ceremony is
//! `docs/goal/behavior/backup-destinations.md` § Third destination kind →
//! *Re-seed*, the gesture `docs/goal/ui/backups.md` § Restore after losing the
//! nest. The leg runs in the app over the owner's own connections and touches
//! no custodian store, so a browser runs it as a native app does.
//!
//! The order and the verdict are the shared driver's
//! ([`fauna_client_backup::reseed::run_reseed`]), the leg the shared
//! [`fauna_client_sync::reseed_pull::NestPullBack`], the post-ceremony duty
//! [`fauna_client_config::reenroll_nest_destination_after_reseed`]. Web
//! supplies only what a browser has — the same four seams the lived-in
//! recovery's door wires (`crate::backup_recover`):
//!
//! * the owner's own authenticated session to the **destination**, held to the
//!   identity the owner enrolled;
//! * the destination's open, hash-verified byte routes
//!   ([`fauna_core::file_download::WasmPublicChunkFetcher`]);
//! * the rebuilt nest's owner-session byte plane
//!   ([`crate::backup_recover::PageBytePlane`]);
//! * this tab's account runtime — the box's backup list, whose coverage rows
//!   name the restored folders, and the folder custody each restored folder's
//!   target set is prepared in and its re-home signed under.
//!
//! Web is never a custodian, so the button's rows are the nest-kind ones alone
//! ([`backup_reseed_rows`] asks with no device id and no orphaned store).

#![cfg(target_arch = "wasm32")]

use std::sync::Arc;

use fauna_client_backup::reseed::{
    DeliveredCorpus, DeliveredSet, FolderRehome, HostedLegs, ReseedDeliveryLeg, result_lines,
    run_reseed,
};
use fauna_client_sync::reseed_pull::{
    NestPullBack, PullBackDestination, PullBackTarget, folder_names_from_coverage,
};
use fauna_core::crypto::{BackupKey, NestBackupKey, OwnerSealKey};
use fauna_core::data::{BackupDestination, DestinationKind};
use fauna_core::file_download::FileDownloadKeys;
use fauna_core::identity::ActorKeypair;
use fauna_core::localized::LocalizedText;
use fauna_rpc_wasm::{TokenWsRpcClient, WsRpcClient as InnerClient};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use crate::backup_recover::PageBytePlane;
use crate::rpc::{WsRpcClient, err_to_js, from_js, to_js};

fauna_client_backup::impl_backup_nest_seam!(struct PullBackDestinationNest<TokenWsRpcClient>);

/// The leg as the driver runs it in the browser: nothing here is `Send`, so
/// the `?Send` arm of the seam, delegating to the shared body (the split
/// `fauna_client_sync::reseed_pull` names).
struct WasmPullBackLeg<'a>(NestPullBack<'a, PageBytePlane, InnerClient>);

#[async_trait::async_trait(?Send)]
impl ReseedDeliveryLeg for WasmPullBackLeg<'_> {
    async fn deliver(&self) -> Result<DeliveredCorpus, String> {
        self.0.deliver_corpus().await
    }

    async fn sign_rehome(&self, set: &DeliveredSet) -> FolderRehome {
        self.0.sign_folder_rehome(set).await
    }
}

/// The fields of a `backupDestinationList` row the restore-source rule reads —
/// the id and what `BackupDestination::kind_view` classifies on. The page hands
/// its rows back as they came; everything else on them is ignored.
#[derive(Deserialize)]
struct ReseedRowJs {
    destination_id: String,
    #[serde(default)]
    destination_nest_url: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    custodian_device_id: Option<String>,
}

fn source_rows(rows: JsValue) -> Result<Vec<BackupDestination>, JsValue> {
    let rows: Vec<ReseedRowJs> = from_js(rows)?;
    Ok(rows
        .into_iter()
        .map(|r| BackupDestination {
            destination_id: r.destination_id,
            destination_nest_url: r.destination_nest_url,
            kind: r.kind,
            custodian_device_id: r.custodian_device_id,
            ..Default::default()
        })
        .collect())
}

/// Where `backup-destination-reseed-button` paints on web — the shared
/// [`fauna_client_backup::reseed::reseed_rows`] over
/// [`HostedLegs::WITH_PULL_BACK`], asked as a browser holding no custodian
/// store: no device id, no orphaned store. Resolves to the destination ids
/// whose `backup-destination-status-row` carries the button.
#[wasm_bindgen(js_name = backupReseedRows)]
pub fn backup_reseed_rows(destinations: JsValue) -> Result<Vec<String>, JsValue> {
    let rows = source_rows(destinations)?;
    Ok(
        fauna_client_backup::reseed::reseed_rows(&rows, "", false, HostedLegs::WITH_PULL_BACK)
            .into_iter()
            .flatten()
            .collect(),
    )
}

/// Whether the confirm armed on `destination_id`'s row runs the nest-held
/// pull-back — the shared
/// [`fauna_client_backup::reseed::reseed_runs_pull_back`], read at press time
/// so the page never re-derives it from the row's kind.
#[wasm_bindgen(js_name = backupReseedRunsPullBack)]
pub fn backup_reseed_runs_pull_back(
    destinations: JsValue,
    destination_id: String,
) -> Result<bool, JsValue> {
    let rows = source_rows(destinations)?;
    Ok(fauna_client_backup::reseed::reseed_runs_pull_back(
        &rows,
        &destination_id,
    ))
}

/// How a pull-back ended, for JS — the wasm twin of the ffi `FfiReseedResult`.
///
/// A record rather than a rejection for the stopped case, because the page
/// renders the two differently: a stop lands on `error-message` inside the
/// `backups.backup_reseed_failed` sentence, a verdict on
/// `backup-destination-reseed-result`.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct WasmReseedResult {
    /// Set when the ceremony stopped before its verdict — nothing was made
    /// live, and re-running resumes it. When set, every other field is empty.
    stopped: Option<String>,
    /// `backup-destination-reseed-result`'s lines, verdict first
    /// ([`result_lines`]); each a `LocalizedText` the SPA resolves with
    /// `resolveLocalizedNested` (a line's set name is itself a key).
    result_lines: Vec<LocalizedText>,
    /// [`fauna_client_backup::reseed::ReseedOutcome::is_whole`] — the one bit
    /// the page may render as "restored".
    is_whole: bool,
    /// The post-ceremony re-enrollment failed: the data IS back, the
    /// destination is not yet backing the restored nest up. The page renders
    /// `backups.backup_reseed_pull_back_reenroll_failed` naming the destination.
    reenroll_error: Option<String>,
}

impl WasmReseedResult {
    fn stopped(reason: impl Into<String>) -> Self {
        Self {
            stopped: Some(reason.into()),
            ..Self::default()
        }
    }
}

#[wasm_bindgen]
impl WsRpcClient {
    /// Restore the signed-in nest from a surviving **nest-kind** destination's
    /// copy — the confirm of `backup-destination-reseed-button` on that
    /// destination's `backup-destination-status-row` (`ui/backups.md`
    /// § Restore after losing the nest). Resolves to
    /// `{ stopped, resultLines, isWhole, reenrollError }`.
    ///
    /// `destination_id` is the pressed row, resolved against the box's **own**
    /// destination list, never a URL the page hands over. A run that stopped
    /// before the driver's verdict resolves with `stopped` set: nothing was
    /// made live, and a re-run resumes by content address. Rejects only for a
    /// secret that is not 32 hex bytes.
    #[wasm_bindgen(js_name = backupPullBackFromDestination)]
    pub fn backup_pull_back_from_destination(
        &self,
        secret_hex: String,
        destination_id: String,
    ) -> js_sys::Promise {
        let client = self.inner_client();
        future_to_promise(async move {
            let secret = fauna_core::hex32::decode(&secret_hex).map_err(err_to_js)?;
            let result = pull_back_from_destination(client, secret, &secret_hex, &destination_id)
                .await
                .unwrap_or_else(WasmReseedResult::stopped);
            to_js(&result)
        })
    }
}

/// The browser's composition, in the native host's order: resolve the row,
/// connect to the destination held to its enrolled identity, name and prepare
/// the covered folders, run the shared driver over the shared leg, then on a
/// whole outcome re-enroll the destination to the restored nest.
async fn pull_back_from_destination(
    target: InnerClient,
    secret: [u8; 32],
    secret_hex: &str,
    destination_id: &str,
) -> Result<WasmReseedResult, String> {
    let js = |e: JsValue| e.as_string().unwrap_or_else(|| format!("{e:?}"));
    let keypair = ActorKeypair::from_secret(secret);
    let actor_hex = keypair.actor_id_hex();

    let restored_nest_id = crate::rpc::bound_nest_id(&target)
        .await
        .map_err(|e| format!("this nest's identity: {}", js(e)))?
        .0;
    // The box's own list, as the Backups page reads it: after a rebuild it is
    // the lost box's (`backup-destinations.md` § *A rebuilt box keeps its list
    // too*), so its coverage rows name the folders the destination holds.
    let store = crate::account_runtime::backup_seam();
    let state = fauna_client_config::load_backup_state_refiled(&*store, &target, restored_nest_id)
        .await
        .map_err(|e| format!("reading this nest's backup list: {e}"))?;
    let destination = state
        .backup
        .destinations
        .iter()
        .find(|d| d.destination_id == destination_id)
        .ok_or_else(|| format!("no backup destination {destination_id}"))?;
    if !matches!(destination.kind_view(), DestinationKind::Nest { .. }) {
        return Err(format!(
            "{destination_id} is not a nest, so there is no copy to pull back over the network"
        ));
    }

    let url = destination.destination_nest_url.trim_end_matches('/');
    let (proven, _domain, dest) =
        crate::rpc::resolve_and_authorize_destination_inner(secret_hex, url)
            .await
            .map_err(js)?;
    if proven != destination.destination_actor_pubkey {
        return Err(format!(
            "{url} proved identity {}, not the destination enrolled as {} — refusing to restore \
             from it",
            fauna_core::hex32::encode(&proven),
            fauna_core::hex32::encode(&destination.destination_actor_pubkey),
        ));
    }
    let device_hex = crate::rpc::this_browsers_device_id(&actor_hex).map_err(|e| e.to_string())?;

    let seal = FileDownloadKeys::owner(OwnerSealKey::Client(BackupKey::derive(&secret)));
    let names = folder_names_from_coverage(
        &state.backup.destinations,
        &destination.destination_id,
        Some(&seal),
    );
    let display_names: Vec<String> = names
        .values()
        .filter_map(|n| n.display_name.clone())
        .collect();
    let folder_keys = crate::account_runtime::folder_key_store(actor_hex);
    let files = fauna_client_folders::FoldersClient::new(target.clone());
    fauna_client_folders::prepare_reseed_targets_logged(&files, &*folder_keys, &display_names)
        .await;

    let seam = PullBackDestinationNest {
        client: fauna_client_backup::BackupClient::new(dest.clone()),
    };
    let fetcher = fauna_core::file_download::WasmPublicChunkFetcher::new(url);
    let sink = PageBytePlane {
        nest: target.clone(),
    };
    let custody: Arc<dyn fauna_client_folders::FolderKeyReader> = folder_keys;
    let leg = WasmPullBackLeg(
        NestPullBack::new(
            PullBackDestination {
                seam: &seam,
                bytes: &fetcher,
            },
            PullBackTarget {
                bytes: &sink,
                nest: &target,
                device_id: device_hex,
            },
            keypair.actor_id().0,
        )
        .with_folder_names(names)
        .with_rehome_signing(fauna_client_folders::record_signing(
            target.clone(),
            &keypair,
            custody,
        )),
    );

    let nest_key = NestBackupKey::derive(&secret);
    let outcome = run_reseed(
        &fauna_client_backup::BackupClient::new(target.clone()),
        nest_key.to_bytes().to_vec(),
        &leg,
    )
    .await
    .map_err(|e| e.to_string())?;
    drop(nest_key);

    let reenroll_error = fauna_client_config::reenroll_nest_destination_after_reseed(
        target,
        dest,
        &*store,
        secret,
        restored_nest_id,
        destination,
        &outcome,
    )
    .await
    .and_then(Result::err)
    .map(|e| e.to_string());
    Ok(WasmReseedResult {
        stopped: None,
        result_lines: result_lines(&outcome),
        is_whole: outcome.is_whole(),
        reenroll_error,
    })
}
