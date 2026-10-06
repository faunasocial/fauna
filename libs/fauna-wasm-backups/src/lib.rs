//! WASM bindings for the Fauna Backups page — the **snapshot half**.
//!
//! One wrapper, [`BackupsMachine`], exposing a JSON-string snapshot getter (the
//! web parses it) + a JS observer shim, mirroring `libs/fauna-wasm-folders`.
//! Loaded by the web Backups page so it renders off the shared machine instead
//! of local Svelte state.
//!
//! The page's *destination* half is unaffected and keeps its existing wasm seams
//! (`WsRpcClient::backup_destination_status`, the `fauna-client-config` mutate
//! helpers) — this chunk is only the folder selector, the snapshot list, and
//! create / delete / prune / check.

use std::sync::Arc;

use fauna_backups_machine::{
    BackupOp, BackupsMachine as InnerMachine, BackupsObserver as InnerObserver, RowIntegrity,
    SnapshotState,
};
use fauna_wasm_panic_hook::err_to_js;
use wasm_bindgen::prelude::*;

/// `fauna_backups_machine::busy_text` → the `in_progress_op` line for a
/// `BackupOp`, so web renders the same text as every other app without
/// re-deriving which key an operation maps to (`docs/goal/ui/backups.md` §
/// Where logic lives). `op` is the same externally-tagged JSON shape the
/// snapshot getter's `in_progress_op` field already carries.
#[wasm_bindgen(js_name = busyText)]
pub fn busy_text(op: JsValue) -> Result<JsValue, JsValue> {
    let op: BackupOp = serde_wasm_bindgen::from_value(op)
        .map_err(|e| JsValue::from_str(&format!("invalid op: {e}")))?;
    serde_wasm_bindgen::to_value(&fauna_backups_machine::busy_text(op)).map_err(err_to_js)
}

/// `fauna_backups_machine::snapshot_state_text` → the `snapshot-item[i]`
/// lifecycle suffix for a `SnapshotState`. `formattedDeadline` is web's own
/// locale-aware rendering of the state's deadline
/// (`behavior/value-formatting.md` § Relative time) — this face owns only the
/// rule (which key, and the dated/undated fallback), never timestamp
/// formatting. `null`/`undefined` when the state is `Active`.
#[wasm_bindgen(js_name = snapshotStateText)]
pub fn snapshot_state_text(
    state: JsValue,
    formatted_deadline: Option<String>,
) -> Result<JsValue, JsValue> {
    let state: SnapshotState = serde_wasm_bindgen::from_value(state)
        .map_err(|e| JsValue::from_str(&format!("invalid state: {e}")))?;
    serde_wasm_bindgen::to_value(&fauna_backups_machine::snapshot_state_text(
        &state,
        formatted_deadline.as_deref(),
    ))
    .map_err(err_to_js)
}

/// `fauna_backups_machine::snapshot_integrity_text` → the `snapshot-item[i]`
/// integrity suffix for a `RowIntegrity`. `null`/`undefined` for
/// `RowIntegrity::Unknown` — absent until a check runs this session, never the
/// word "unknown" (`docs/goal/ui/backups.md` § Snapshot-list shape, *Check*).
#[wasm_bindgen(js_name = snapshotIntegrityText)]
pub fn snapshot_integrity_text(integrity: JsValue) -> Result<JsValue, JsValue> {
    let integrity: RowIntegrity = serde_wasm_bindgen::from_value(integrity)
        .map_err(|e| JsValue::from_str(&format!("invalid integrity: {e}")))?;
    serde_wasm_bindgen::to_value(&fauna_backups_machine::snapshot_integrity_text(integrity))
        .map_err(err_to_js)
}

#[wasm_bindgen]
extern "C" {
    pub type JsBackupsObserver;
    #[wasm_bindgen(method, js_name = onChanged)]
    fn on_changed(this: &JsBackupsObserver);
}

struct ObserverShim(JsBackupsObserver);
// SAFETY: wasm32 is single-threaded; the JS object never crosses a thread.
// Send + Sync are required by the trait bounds but never exercised at runtime.
unsafe impl Send for ObserverShim {}
unsafe impl Sync for ObserverShim {}

impl InnerObserver for ObserverShim {
    fn on_changed(&self) {
        self.0.on_changed();
    }
}

#[wasm_bindgen]
pub struct BackupsMachine(Arc<InnerMachine>);

/// The 32 secret bytes behind a hex identity secret, or `None` when the caller
/// had none / handed over something unusable. Separate from the constructor so
/// the length check cannot be dropped by a later edit: a 31-byte "secret" would
/// otherwise panic in `BackupKey::derive`, taking the whole page down over a
/// label-opening nicety.
fn owner_secret_bytes(owner_secret_hex: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(owner_secret_hex).ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

#[wasm_bindgen]
impl BackupsMachine {
    /// Build the page machine over the SPA core chunk's socket, lent as
    /// `port` (a `SharedRpcPort` — `$lib/rpc`'s `sharedRpcPort`; the owner's
    /// `requestRaw` runs every request this machine makes, so web keeps one
    /// WebSocket per actor). Throws on an object that is not a port. State
    /// starts empty; call `refresh()` to populate it.
    ///
    /// **Custody — resolver-backed, from `ownerSecretHex`, and it is
    /// load-bearing.** The browser *does* hold the identity secret (every other
    /// snapshot call on this page already passes it), so both arms of
    /// [`fauna_client_folders::seal_backfill::resolver_backed_custody`] are
    /// derivable here: the owner `BackupKey`, and — since `NestFolderKeyResolver`
    /// became transport-generic (2026-08-11) — the shared-set resolver, over the
    /// same `WsRpcClient` this constructor already opens. Handing this machine
    /// keyless custody would reintroduce bug: a sealed set's
    /// `snapshot-detail-files` renders EMPTY for a reader who in fact holds the
    /// key. Handing it owner-only custody instead — this constructor's shape
    /// until 2026-08-13 — is a known gap: a set *bound* to this actor (shared
    /// key, not owned) would silently seal under a root no roster member can
    /// open, which looks like a graceful degrade and is not; the resolver arm is
    /// what makes a bound-but-unresolvable set fail **closed** instead, exactly
    /// as every native app's snapshot browse already does
    /// (`fauna-ffi`'s `owner_read_snapshots_client`) and as web's own
    /// seal-backfill sweep does (`runSealBackfillSweep`, `rpc.rs`).
    ///
    /// This is the **read-direction** shape, deliberately, and it is not the seal-side trap: the machine's only writing call is `create_folder` with an
    /// empty tag list, and `SnapshotsClient::seal_tags` returns before touching
    /// custody on an empty list — so the owner root can render more and can
    /// mis-seal nothing. (The UniFFI face states the same rule for native.)
    ///
    /// An absent, unparseable, or wrong-length `ownerSecretHex` degrades to keyless
    /// custody rather than failing the build — the page still renders, sealed labels
    /// simply do not open.
    #[wasm_bindgen(constructor)]
    pub fn new(
        observer: JsBackupsObserver,
        port: fauna_rpc_wasm::JsRpcPort,
        owner_secret_hex: String,
        account_port: fauna_account_port::JsAccountPort,
    ) -> Result<BackupsMachine, JsValue> {
        let observer: Arc<dyn InnerObserver> = Arc::new(ObserverShim(observer));
        let client = fauna_rpc_wasm::WsRpcClient::over_port(port.into())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        // The shared-set resolver reads the account's folder-key custody
        // through the tab's account port (`fauna_client_folders::port`).
        let transport = fauna_account_port::JsAccountTransport::new(account_port.into())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let folder_keys: Arc<dyn fauna_client_folders::FolderKeyReader> =
            Arc::new(fauna_client_folders::port::PortFolderKeys::new(transport));
        let custody = owner_secret_bytes(&owner_secret_hex).map_or_else(
            fauna_core::label_custody::LabelCustody::default,
            |secret| {
                fauna_client_folders::seal_backfill::resolver_backed_custody(
                    client.clone(),
                    &fauna_core::identity::ActorKeypair::from_secret(secret),
                    folder_keys,
                )
            },
        );
        Ok(BackupsMachine(
            fauna_backups_machine::build_backups_machine(client, observer, custody),
        ))
    }

    /// Wire the shell's stable sync device id, so manual snapshots carry row
    /// provenance. Hex; an unparseable or wrong-length value clears it (an
    /// unattributed snapshot beats a wrong provenance).
    #[wasm_bindgen(js_name = setDeviceId)]
    pub fn set_device_id(&self, device_id_hex: String) {
        self.0
            .set_device_id(hex::decode(&device_id_hex).ok().filter(|b| b.len() == 32));
    }

    // ── Reads ───────────────────────────────────────────────────────────

    /// The whole renderable page state as JSON — `BackupsSnapshot`. The page
    /// re-reads this on every observer tick and renders off it.
    #[wasm_bindgen(js_name = snapshotJson)]
    pub fn snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.snapshot()).unwrap_or_default()
    }

    /// `immediate-delete-confirm-button`'s enabled flag, with the machine's real
    /// in-flight state threaded in. The page binds this; it never re-derives the
    /// predicate (Architectural rule 4).
    #[wasm_bindgen(js_name = immediateDeleteEnabled)]
    pub fn immediate_delete_enabled(
        &self,
        confirm_id: String,
        target_id: String,
        acknowledge: String,
    ) -> bool {
        self.0
            .immediate_delete_enabled(confirm_id, target_id, acknowledge)
    }

    // ── Gestures ────────────────────────────────────────────────────────

    #[wasm_bindgen(js_name = refresh)]
    pub fn refresh(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.refresh().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    #[wasm_bindgen(js_name = selectFolder)]
    pub fn select_folder(&self, name: String) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.select_folder(name).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    #[wasm_bindgen(js_name = createSnapshot)]
    pub fn create_snapshot(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.create_snapshot().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// ⚠ `snapshot_id` is `f64`, not `i64`: an `i64` parameter arrives from JS
    /// as a `BigInt` and throws a `TypeError` for an ordinary JS number, which
    /// is what every call site here has. Rounded back to `i64` inside.
    #[wasm_bindgen(js_name = deleteSnapshot)]
    pub fn delete_snapshot(&self, snapshot_id: f64) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.delete_snapshot(snapshot_id as i64).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `snapshot-undelete-button[i]` — recover a soft-deleted row before its
    /// `purge_after`. The machine refuses any row that is not `SoftDeleted`, so
    /// the page never has to re-derive the rule. See [`Self::delete_snapshot`]
    /// on the `f64` id.
    #[wasm_bindgen(js_name = undeleteSnapshot)]
    pub fn undelete_snapshot(&self, snapshot_id: f64) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.undelete_snapshot(snapshot_id as i64).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// See [`Self::delete_snapshot`] on the `f64` id.
    #[wasm_bindgen(js_name = deleteSnapshotImmediate)]
    pub fn delete_snapshot_immediate(
        &self,
        snapshot_id: f64,
        confirm_id: String,
        acknowledge: String,
    ) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner
                .delete_snapshot_immediate(snapshot_id as i64, confirm_id, acknowledge)
                .await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// The prune **preview** (a dry run of the set's own resting policy). The
    /// web page's current `dry_run = false` hard-coding dies with this.
    #[wasm_bindgen(js_name = prunePreview)]
    pub fn prune_preview(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.prune_preview().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Execute the previewed prune. A no-op without a standing preview.
    #[wasm_bindgen(js_name = pruneExecute)]
    pub fn prune_execute(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.prune_execute().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    #[wasm_bindgen(js_name = cancelPrunePreview)]
    pub fn cancel_prune_preview(&self) {
        self.0.cancel_prune_preview();
    }

    #[wasm_bindgen(js_name = check)]
    pub fn check(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.check().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Open one snapshot's file list (`snapshot-detail-files`) — the
    /// custody-wired sealed-plane read. See [`Self::delete_snapshot`] on the
    /// `f64` id.
    #[wasm_bindgen(js_name = openSnapshot)]
    pub fn open_snapshot(&self, snapshot_id: f64) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.open_snapshot(snapshot_id as i64).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Close the open file list. Local state only — no round trip.
    #[wasm_bindgen(js_name = closeSnapshotDetail)]
    pub fn close_snapshot_detail(&self) {
        self.0.close_snapshot_detail();
    }

    #[wasm_bindgen(js_name = clearError)]
    pub fn clear_error(&self) {
        self.0.clear_error();
    }
}

// Each wasm chunk is its own runtime, so the panic hook must be installed
// per-chunk. `#[wasm_bindgen(start)]` runs automatically the moment this
// chunk's module is instantiated — no SPA-side call site to remember.
#[wasm_bindgen(start)]
fn panic_hook_start() {
    fauna_wasm_panic_hook::install("fauna-wasm-backups");
}

/// Test-only: deliberately panics, so an e2e can assert the hook above really
/// names this chunk in the browser console — a headless witness, not a
/// review-only claim. Compiled out of every non-`test-helpers` build.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = panicForTestOnly)]
pub fn panic_for_test_only() {
    panic!("deliberate test panic");
}
