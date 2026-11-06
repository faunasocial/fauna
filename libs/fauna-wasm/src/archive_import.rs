//! The `archive-import` page's face — the shared
//! [`fauna_archive_import_machine::ArchiveImportMachine`] over the browser
//! plane (`docs/goal/behavior/archive-import.md` § The wizard and its machine).
//!
//! In the core chunk, not a page chunk of its own: the core chunk registers
//! the `Archive` rail and the folded comments over the same machine crate and
//! plane, and `zip` is already in its graph (`fauna-mail`), so a separate chunk
//! would only ship the machine and the parser twice.
//!
//! [`WasmArchiveImportMachine`] (built by `WsRpcClient::archiveImportMachine`):
//! the snapshot, the page's composed lines
//! ([`fauna_archive_import_machine::text::page_text`], which the SPA resolves
//! with `resolveComposed` — it never fills the `archive_import.*_fmt` templates
//! itself), hydrate, dispatch and the run loop; the shared label maps are free
//! functions beside it. [`WasmArchiveLinks`] (built and registered as the
//! FaunaMls backend's archive-signal sink by `WsRpcClient::registerArchiveLinks`)
//! is phase two's per-account link module, which the Contacts rows paint and
//! the machine is attached to (`attachLinks`).
//!
//! **The archive is held in memory.** The browser hands the page a `File`; a
//! synchronous range read of it is impossible on the one thread the zip reader
//! runs on, so the page reads it whole and hands the bytes over with
//! [`WasmArchiveImportMachine::set_archive_file`], keyed by the path the page
//! then dispatches (`SetArchivePath`) — the shared
//! [`fauna_archive_import_machine::MemoryOpener`].

use std::sync::Arc;

use fauna_archive_import_machine::comments::register_archive_comments;
use fauna_archive_import_machine::link::{ArchiveLinkView, ArchiveLinks, BackendPoster};
use fauna_archive_import_machine::rpc_glue::{
    build_archive_import_machine_over, build_archive_nest_over,
};
use fauna_archive_import_machine::threads::register_archive_threads;
use fauna_archive_import_machine::web_plane::WebPlane;
use fauna_archive_import_machine::{
    ArchiveImportAction, ArchiveImportMachine, ArchiveNest, ArchiveOpener, ArchiveSourceKind,
    AudienceMode, MemoryOpener, RunState,
};
use fauna_rpc_wasm::WsRpcClient as InnerClient;
use wasm_bindgen::prelude::*;

use crate::rpc::{err_to_js, from_js, to_js};

#[wasm_bindgen]
pub struct WasmArchiveImportMachine {
    inner: Arc<ArchiveImportMachine>,
    opener: Arc<MemoryOpener>,
}

impl WasmArchiveImportMachine {
    /// Build over the session's socket and this tab's custody (period keys,
    /// the account's folder keys, mail). `owner_secret_hex` is the identity
    /// secret the archive folder seals under and every imported post is
    /// signed with; `device_id_hex` the sync device id every change record is
    /// stamped with. `Err` on a malformed secret or device id — the page then
    /// shows the unavailable notice instead of a wizard that could not commit.
    pub(crate) fn build(
        client: InnerClient,
        actor_id_hex: String,
        owner_secret_hex: &str,
        device_id_hex: &str,
    ) -> Result<Self, JsValue> {
        let (plane, device_id) = plane_for(&client, owner_secret_hex, device_id_hex)?;
        let opener = Arc::new(MemoryOpener::default());
        let inner = build_archive_import_machine_over(
            client,
            plane,
            device_id,
            crate::account_runtime::period_key_store(),
            crate::account_runtime::folder_key_store(actor_id_hex),
            crate::account_runtime::mail_store(),
            Arc::clone(&opener) as Arc<dyn ArchiveOpener>,
        )
        .map_err(err_to_js)?;
        Ok(Self {
            inner: Arc::new(inner),
            opener,
        })
    }
}

/// The browser plane the archive folder seals and signs under, and the device
/// id its change records carry — the machine's and the two read seams' one
/// decode. `Err` on a malformed secret or device id.
fn plane_for(
    client: &InnerClient,
    owner_secret_hex: &str,
    device_id_hex: &str,
) -> Result<(Arc<WebPlane>, [u8; 32]), JsValue> {
    let seed = zeroize::Zeroizing::new(
        fauna_core::hex32::decode(owner_secret_hex)
            .map_err(|e| JsValue::from_str(&format!("owner secret: {e}")))?,
    );
    let device_id = fauna_core::hex32::decode(device_id_hex)
        .map_err(|e| JsValue::from_str(&format!("device id: {e}")))?;
    Ok((Arc::new(WebPlane::new(client.clone(), seed)), device_id))
}

/// The account's archive folders over this session's socket — what the
/// `Archive` rail ([`register_archive_threads`]) and the comment fold
/// ([`register_archive_comments`]) read, the same seam the wizard writes
/// through (`archive-import.md` § What each category becomes).
fn archive_nest(
    client: InnerClient,
    actor_id_hex: String,
    owner_secret_hex: &str,
    device_id_hex: &str,
) -> Result<Arc<dyn ArchiveNest>, JsValue> {
    let (plane, device_id) = plane_for(&client, owner_secret_hex, device_id_hex)?;
    build_archive_nest_over(
        client,
        plane,
        device_id,
        crate::account_runtime::period_key_store(),
        crate::account_runtime::folder_key_store(actor_id_hex),
        crate::account_runtime::mail_store(),
    )
    .map_err(err_to_js)
}

/// Register the account's imported threads as `manager`'s `Archive` rail
/// source, through the shared door every app calls. Reads nothing: the page
/// re-reads with `WasmConversationsManager::refreshArchiveThreads`.
pub(crate) fn register_threads(
    client: InnerClient,
    actor_id_hex: String,
    manager: &fauna_conversations::ConversationsManager,
    owner_secret_hex: &str,
    device_id_hex: &str,
) -> Result<(), JsValue> {
    let archive = archive_nest(client, actor_id_hex, owner_secret_hex, device_id_hex)?;
    register_archive_threads(manager, archive);
    Ok(())
}

/// Register the account's folded archive comments as `manager`'s comment
/// source, through the shared door every app calls. Reads nothing: the feed
/// re-reads with `WasmFeedManager::refreshArchiveComments`.
pub(crate) fn register_comments(
    client: InnerClient,
    actor_id_hex: String,
    manager: &fauna_feed::FeedManager<InnerClient>,
    owner_secret_hex: &str,
    device_id_hex: &str,
) -> Result<(), JsValue> {
    let archive = archive_nest(client, actor_id_hex, owner_secret_hex, device_id_hex)?;
    register_archive_comments(manager, archive);
    Ok(())
}

/// Phase two's per-account link module (`archive-import.md` § Linking and
/// merging, ruling 8) — the same six calls every app makes, over the shared
/// [`ArchiveLinks`]. Built by `WsRpcClient::registerArchiveLinks`, which also
/// registers it as the FaunaMls backend's archive-signal sink.
#[wasm_bindgen]
pub struct WasmArchiveLinks {
    inner: Arc<ArchiveLinks>,
}

/// Build the account's [`ArchiveLinks`] over the archive seam the `Archive`
/// rail reads and register it as `manager`'s FaunaMls backend's archive-signal
/// sink (ruling 2) — web's `wire_archive_links`. The poster is the backend's
/// own doors ([`BackendPoster`]): web has no `ConversationsSession`. The
/// backend keeps the first sink it is given, so this is called once per
/// manager build. `Err` on a manager built without the FaunaMls rail, or a
/// malformed secret or device id.
pub(crate) fn register_links(
    client: InnerClient,
    actor_id_hex: String,
    manager: &crate::conversations::WasmConversationsManager,
    owner_secret_hex: &str,
    device_id_hex: &str,
) -> Result<WasmArchiveLinks, JsValue> {
    let (shared, backend) = manager
        .account_seam_parts()
        .ok_or_else(|| JsValue::from_str("archive links: no FaunaMls rail on this manager"))?;
    let seed = zeroize::Zeroizing::new(
        fauna_core::hex32::decode(owner_secret_hex)
            .map_err(|e| JsValue::from_str(&format!("owner secret: {e}")))?,
    );
    let keypair = fauna_core::identity::ActorKeypair::from_secret(*seed);
    let archive = archive_nest(client, actor_id_hex, owner_secret_hex, device_id_hex)?;
    #[allow(clippy::arc_with_non_send_sync)]
    // wasm is single-threaded; the seams are `Arc` natively
    let links = Arc::new(ArchiveLinks::new(
        archive,
        Arc::new(BackendPoster::new(&backend, &shared)),
        &keypair,
    ));
    backend.set_archive_signal_sink(Arc::clone(&links) as Arc<_>);
    Ok(WasmArchiveLinks { inner: links })
}

#[wasm_bindgen]
impl WasmArchiveLinks {
    /// `ArchiveLinks::link_views` — one `ArchiveLinkView` per pair, as plain
    /// objects. Read when [`Self::captures`] moved, and with the roster.
    #[wasm_bindgen(js_name = linkViews)]
    pub fn link_views(&self) -> js_sys::Promise {
        let l = Arc::clone(&self.inner);
        wasm_bindgen_futures::future_to_promise(async move {
            let views = l.link_views().await.map_err(err_to_js)?;
            to_js(&views)
        })
    }

    /// `contact-archive-link-yes` / `-no`: answer a view's question once.
    #[wasm_bindgen(js_name = answerLink)]
    pub fn answer_link(&self, contact: String, platform: String, yes: bool) -> js_sys::Promise {
        let l = Arc::clone(&self.inner);
        wasm_bindgen_futures::future_to_promise(async move {
            l.answer_link_token(contact, platform, yes)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `contact-archive-unlink`.
    #[wasm_bindgen]
    pub fn unlink(&self, contact: String, platform: String) -> js_sys::Promise {
        let l = Arc::clone(&self.inner);
        wasm_bindgen_futures::future_to_promise(async move {
            l.unlink_token(contact, platform).await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// One drive pass (ruling 4) — once at login, and on the tick whenever
    /// [`Self::work_due`] reads true.
    #[wasm_bindgen]
    pub fn drive(&self) -> js_sys::Promise {
        let l = Arc::clone(&self.inner);
        wasm_bindgen_futures::future_to_promise(async move {
            l.drive_links().await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Whether a captured signal left work owed — an atomic load.
    #[wasm_bindgen(js_name = workDue)]
    pub fn work_due(&self) -> bool {
        self.inner.link_work_due()
    }

    /// The sink's capture count — moved since the last look → re-read
    /// [`Self::link_views`] and repaint the Contacts rows. `f64`: a JS number.
    #[wasm_bindgen]
    pub fn captures(&self) -> f64 {
        self.inner.link_captures() as f64
    }
}

/// `contact-archive-link-prompt`'s words — `text::link_prompt` as
/// `ComposedText` JSON. `view` is one of `linkViews`' objects;
/// `contactName` the row's display name.
#[wasm_bindgen(js_name = archiveLinkPromptJson)]
pub fn link_prompt_json(view: JsValue, contact_name: String) -> Result<String, JsValue> {
    let view: ArchiveLinkView = from_js(view)?;
    serde_json::to_string(&fauna_archive_import_machine::text::link_prompt(
        &view,
        &contact_name,
    ))
    .map_err(err_to_js)
}

/// `contact-archive-linked`'s words — `text::linked_label` as `ComposedText`
/// JSON.
#[wasm_bindgen(js_name = archiveLinkedLabelJson)]
pub fn linked_label_json(view: JsValue) -> Result<String, JsValue> {
    let view: ArchiveLinkView = from_js(view)?;
    serde_json::to_string(&fauna_archive_import_machine::text::linked_label(&view))
        .map_err(err_to_js)
}

#[wasm_bindgen]
impl WasmArchiveImportMachine {
    /// Attach the account's links (`ArchiveImportMachine::attach_links`)
    /// before a Start or Resume: a run that completes then puts the link
    /// counts on the snapshot and runs the announce round itself (rulings 4
    /// and 11), whichever page the user is on by then.
    #[wasm_bindgen(js_name = attachLinks)]
    pub fn attach_links(&self, links: &WasmArchiveLinks) {
        self.inner.attach_links(Arc::clone(&links.inner));
    }

    /// The page's state — `ArchiveImportSnapshot`, as a plain object.
    #[wasm_bindgen]
    pub fn snapshot(&self) -> Result<JsValue, JsValue> {
        to_js(&self.inner.snapshot())
    }

    /// The page's lines for the current snapshot —
    /// `fauna_archive_import_machine::text::page_text` as JSON, each line a
    /// `ComposedText` the SPA resolves with `resolveComposed`.
    #[wasm_bindgen(js_name = pageTextJson)]
    pub fn page_text_json(&self) -> String {
        let text = fauna_archive_import_machine::text::page_text(&self.inner.snapshot());
        serde_json::to_string(&text).unwrap_or_default()
    }

    /// Hold the picked archive's bytes under `path` (the page then dispatches
    /// `{ SetArchivePath: { value: path } }` and `OpenArchive`).
    #[wasm_bindgen(js_name = setArchiveFile)]
    pub fn set_archive_file(&self, path: String, bytes: Vec<u8>) {
        self.opener.insert(&path, bytes);
    }

    #[wasm_bindgen]
    pub fn hydrate(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.inner);
        wasm_bindgen_futures::future_to_promise(async move {
            m.hydrate().await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Dispatch one `ArchiveImportAction` (its serde shape: `"Next"`,
    /// `{ SelectSource: { kind: "Instagram" } }`, …). After a `Start` or a
    /// `Resume` that leaves the run `Running`, the page calls
    /// [`Self::run_import`] — the authoring loop stops on Pause and needs a
    /// fresh call to go on.
    #[wasm_bindgen]
    pub fn dispatch(&self, action: JsValue) -> js_sys::Promise {
        let m = Arc::clone(&self.inner);
        wasm_bindgen_futures::future_to_promise(async move {
            let action: ArchiveImportAction = from_js(action)?;
            m.dispatch(action).await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Whether the run should be driven now: the snapshot reads `Running`.
    #[wasm_bindgen(js_name = isRunning)]
    pub fn is_running(&self) -> bool {
        self.inner.snapshot().run_state == Some(RunState::Running)
    }

    /// Drive the import run until it pauses, stops or completes. Single-flight
    /// in the machine: a second call while one is in flight is refused.
    #[wasm_bindgen(js_name = runImport)]
    pub fn run_import(&self) -> js_sys::Promise {
        let m = Arc::clone(&self.inner);
        wasm_bindgen_futures::future_to_promise(async move {
            m.run_import().await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// The `archive_import_pause_after` e2e anchor (testing.md convention 15:
    /// the core chunk's `test-helpers` flavor only).
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = setPauseAfterRecordsForTest)]
    pub fn set_pause_after_records_for_test(&self, records: u32) {
        self.inner
            .set_pause_after_records_for_test(u64::from(records));
    }

    /// The `archive_import_hold_after` e2e anchor.
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = setHoldAfterRecordsForTest)]
    pub fn set_hold_after_records_for_test(&self, records: u32) {
        self.inner
            .set_hold_after_records_for_test(u64::from(records));
    }
}

// ── the shared label maps ───────────────────────────────────────────────────

/// `archive_import_source_kind_label` — the source picker's option label.
#[wasm_bindgen(js_name = archiveImportSourceKindLabel)]
pub fn source_kind_label(kind: JsValue) -> Result<String, JsValue> {
    let kind: ArchiveSourceKind = from_js(kind)?;
    Ok(fauna_archive_import_machine::archive_import_source_kind_label(kind))
}

/// `archive_import_source_help` — the Source step's help line (`LocalizedText`).
#[wasm_bindgen(js_name = archiveImportSourceHelp)]
pub fn source_help(kind: JsValue) -> Result<JsValue, JsValue> {
    let kind: ArchiveSourceKind = from_js(kind)?;
    to_js(&fauna_archive_import_machine::archive_import_source_help(
        kind,
    ))
}

/// `archive_import_audience_mode_label` — an audience option's label.
#[wasm_bindgen(js_name = archiveImportAudienceModeLabel)]
pub fn audience_mode_label(mode: JsValue) -> Result<JsValue, JsValue> {
    let mode: AudienceMode = from_js(mode)?;
    to_js(&fauna_archive_import_machine::archive_import_audience_mode_label(mode))
}
