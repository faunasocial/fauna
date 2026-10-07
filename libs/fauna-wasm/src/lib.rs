//! WASM bindings for Fauna crypto and encoding.
//!
//! Exposes functions to the browser via wasm-bindgen:
//! - `generate_keypair()` → hex secret
//! - `actor_id_from_secret(hex)` → hex public key
//! - `get_recipients(payload)` → JSON string of hex actor IDs
//! - auth/register request builders
//! - MLS key package generation
//!
//! ## Gated element ids live in gated DOC LINES
//!
//! Several formatters exported here are ungated inert surfaces that a *gated*
//! plane's renders call — the tip formatters and `claim_status_label` are
//! `payments` (`dynamic-features.md` § Charter members). Naming their element
//! ids in ungated prose ships those ids in a store-safe artifact:
//! `#[wasm_bindgen]` records each exported item's doc comment in the
//! `__wasm_bindgen_unstable` metadata section so `wasm-bindgen-cli` can emit it
//! as JSDoc on the generated face — so the prose rides in the `.wasm` itself and
//! again in `fauna_wasm.js`, and an excised build would carry — and *document* —
//! the UI it excised. Criterion 1 is "prose included" exactly as criterion 2 is
//! (`dynamic-features.md` § What "completely compiled away" means).
//!
//! **This is the same defect class as the UniFFI one row 310 fixed at the
//! `fauna-ffi` root, with a different proc macro carrying it** — and the ffi
//! column's green was never evidence about this root (§ The wasm root's own
//! column: one witness per flavor root, built for that root's own target).
//! Measured here 2026-08-18 on the store-safe artifact: `post-tip-` **3**,
//! `subscription-claim-` **1**, all four doc lines, now **0**.
//!
//! So the id-naming sentence rides `#[cfg_attr(feature = "payments", doc = …)]`
//! while the rest of the doc stays ungated. **Gate the line; never reword it** —
//! the kebab id stays spelled verbatim in this file, so `rg post-tip-total`
//! still finds every driver. `just wasm-store-safe-check` pins both halves: the
//! ids absent from the store-safe flavor, present in the default one.

// Several seam traits consumed here (`ConversationsRpc`, `FolderGateSink`,
// `MlsReplicaTransport`'s `MlsStateSync` owner, `OutboundMailSink`'s
// `SmtpBackend` owner, …) are bounded by `MaybeSendSync` (`Send + Sync`
// natively, empty on wasm32 — `fauna_core::maybe_send`). On wasm32 the
// `Arc<dyn ...>`-holding types this crate constructs are correctly
// `!Send`/`!Sync` (wasm is single-threaded, nothing crosses a real thread
// here) but that trips `arc_with_non_send_sync`. wasm32-scoped so native
// builds of any code sharing this pattern keep the lint's protection.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]

use wasm_bindgen::prelude::*;

use fauna_core::data::{ContentHash, MediaItem, Post, Timestamp};
use fauna_core::identity::ActorKeypair;

pub mod conversations;
pub use conversations::*;

pub mod wrapped_blob;
pub use wrapped_blob::*;

pub mod subscription;
pub use subscription::*;

pub mod feed;
pub use feed::*;

// The Events page's `"events"` drafts rail — the third and last constant of
// `fauna_protocol::drafts::DRAFT_RAILS`. Unlike the other two rails this page
// has no shared manager, so the face carries the record itself
// (`reserved-folders.md` § Drafts Sync).
pub mod event_drafts;
pub use event_drafts::*;

// The Search page's shared manager (`fauna_client_search::SearchManager`) —
// wasm-only, like `rpc`, because it depends on `fauna-rpc-wasm`'s `Rc`-based
// browser transport (`WsRpcClient`), which only exists on wasm. The local
// arm (backend 2, sealed tantivy) never registers here — see `search`'s
// module docs.
#[cfg(target_arch = "wasm32")]
pub mod search;
#[cfg(target_arch = "wasm32")]
pub use search::*;

// On-device per-user mail spam scorer (`fauna_mail::spam` via `spam-classifier`)
// — the browser twin of the Go MDA's INBOX scoring, so a web-primary user gets
// the same per-user Junk filtering. Pure (no wasm-sys); ungated like `feed`/
// `conversations`. content-scoring.md § The shared-scorer shape (priority #2).
pub mod spam;
pub use spam::*;

// Calendar/date math (`fauna_core::caltime`) for the SPA's events/calendar
// views — the wasm twin of linux's `time_utils::*` (priority #2/#4). Gated
// `wasm32` like the other modules that route returns through `crate::rpc::to_js`;
// natives stay on their platform date libraries (no UniFFI twin — events.md
// § Where logic lives).
#[cfg(target_arch = "wasm32")]
pub mod caltime;
#[cfg(target_arch = "wasm32")]
pub use caltime::*;

// Web nest-identity TOFU pin glue — wasm-only (depends on
// web-sys `Storage`). The localStorage pin store + the `forgetNestIdentityPin`
// recovery export; the pure pin/compare logic is shared
// (`fauna_client_core::nest_trust`). security.md § Transport trust.
#[cfg(target_arch = "wasm32")]
pub mod nest_identity;
#[cfg(target_arch = "wasm32")]
pub use nest_identity::*;

// Multi-account registry glue (Stage 1 web) — wasm-only (depends on web-sys
// `Storage`). The `LocalStorageSecretStore` + `WasmAccountRegistry` the Svelte
// account-settings switcher drives; the shared account list / active pointer
// is `fauna_client_accounts` (priority #2).
// long-term-store.md § Multi-account evolution.
#[cfg(target_arch = "wasm32")]
pub mod accounts;
#[cfg(target_arch = "wasm32")]
pub use accounts::*;

// This tab's account runtime — the SPA's host of the account driver over the
// IndexedDB store (`fauna_account_plane::web_host`), started once the
// conversations manager exists; account-client-lifecycle.md § The client-side
// lifecycle → The trigger fired, ruling (4).
#[cfg(target_arch = "wasm32")]
pub mod account_runtime;

// Web's account-scope erase — what a sign-out and a remove-account erase for
// an account beyond the registry's own slots (the account store), behind the
// sign-out record a later load finishes; account-scoping.md § The scoping
// taxonomy → Erasure follows scope, the web paragraph.
#[cfg(target_arch = "wasm32")]
pub mod account_scope;

// The account port's core half: `accountPortCall`, which answers a door of a
// consumer seam for a page machine in another chunk from this tab's runtime;
// account-client-lifecycle.md § The client-side lifecycle → The account port.
#[cfg(target_arch = "wasm32")]
pub mod account_port;

// The backup audit loop's client-local state on web: a `localStorage`-backed
// `AuditStateStore` + the conversation-list observation feed. Wasm-only
// (`web_sys::Storage`); the browser twin of linux/tui's `backup_audit.rs`. See
// `docs/goal/ui/backups.md` § Audit-alert surface.
#[cfg(target_arch = "wasm32")]
pub mod backup_audit;

// Browser WS-RPC façade — wasm-only (depends on `fauna-rpc-wasm`, which only
// exists on wasm). Exposes the `WsRpcClient` handle + typed bridges/email
// methods. Phase 4 of the WS-RPC adoption migration (tracked internally).
#[cfg(target_arch = "wasm32")]
pub mod rpc;
#[cfg(target_arch = "wasm32")]
pub use rpc::*;

// The region content plane's app side (`region-blocking.md` § The content
// plane) — `WasmRegionPlane`, the web twin of the UniFFI `FfiRegionPlane`.
// Wasm-only, same reason as `rpc` (its refresh rides `WsRpcClient`).
#[cfg(target_arch = "wasm32")]
pub mod region;

// The feature plane's authoring editor (`dynamic-features.md` § Authoring
// surfaces) — `WasmPolicyEditor`, the web twin of the UniFFI `FfiPolicyEditor`.
// Wasm-only, same reason as `rpc` (its reads and writes ride `WsRpcClient`).
#[cfg(target_arch = "wasm32")]
pub mod feature_editor;

// This chunk's own critical-alerts registry — the session-start sweep's
// second web source. Wasm-only, same
// reason as `rpc` (`WsRpcClient::runCriticalAlertSweep` in that module is
// this registry's only writer).
#[cfg(target_arch = "wasm32")]
pub mod critical_alerts;
#[cfg(target_arch = "wasm32")]
pub use critical_alerts::*;

// The connection-report and painted-error counters behind the connection-gap
// journeys (`fauna_e2e_agent::{CONNECTION_REPORTS_KEY, PAINTED_ERRORS_KEY}`) —
// the test bundle only (convention 15).
#[cfg(all(target_arch = "wasm32", feature = "test-helpers"))]
pub mod e2e_observables;

// The Backups per-file download: the browser binding of the shared
// `fauna_core::file_download` walk (the twin of the sync engine's native
// `EngineBlobFetcher`). Wasm-only — its blob-fetch leg is `gloo-net`. See
// `docs/goal/ui/backups.md` § Where logic lives → *Single-file byte download*.
#[cfg(target_arch = "wasm32")]
pub mod snapshot_download;
#[cfg(target_arch = "wasm32")]
pub use snapshot_download::*;

// Web log-ring exposure (the browser twin of `fauna-ffi/src/logs.rs`): the
// ring-only subscriber install + snapshot/clear reads behind the Settings → Logs
// page. Wasm-only — `web-sys` / the subscriber install only exist on wasm. See
// `docs/goal/architecture/apps/observability.md` § Persistence & privacy.
#[cfg(target_arch = "wasm32")]
pub mod logs;
#[cfg(target_arch = "wasm32")]
pub use logs::*;

// The `wasm_admin_machine!` macro shared by the state-machine wrappers below.
// `#[macro_use]` + declared before them so `mail_admin` / `pairing` can invoke it.
#[cfg(target_arch = "wasm32")]
#[macro_use]
mod wasm_admin_machine;

// Admin mail/DNS state machines (the wasm twin of `fauna-ffi/src/mail_admin.rs`).
// Wasm-only, same reason as `rpc` — the shared seams' wasm transport is
// `fauna-rpc-wasm`. Tracked internally, Slice 2.
#[cfg(target_arch = "wasm32")]
pub mod mail_admin;
// The browser half of `mail-export.md` § Download flow, behind the page's save
// port — what `WasmMailExportMachine`'s custody build downloads through.
#[cfg(target_arch = "wasm32")]
mod mail_export_delivery;
#[cfg(target_arch = "wasm32")]
pub use mail_admin::*;

// User-settings Linked-nests machine (the wasm twin of `fauna-ffi/src/pairing.rs`).
// Wasm-only, same reason as `mail_admin` (tracked internally).
#[cfg(target_arch = "wasm32")]
pub mod pairing;
#[cfg(target_arch = "wasm32")]
pub use pairing::*;

// User-settings Task-delegation surface (the wasm twin of
// `fauna-ffi/src/task_delegation.rs`). Wasm-only, same reason as `pairing` — the
// shared `TaskDelegationView`'s transport is `fauna-rpc-wasm`.
#[cfg(target_arch = "wasm32")]
pub mod task_delegation;
#[cfg(target_arch = "wasm32")]
pub use task_delegation::*;

// The succession ceremony's web-side orchestration — the browser twin of
// `fauna_client_recovery::ceremony`'s three native functions, which cannot be
// shared because they dial transports a browser does not have (the outcome
// types and the undecidable-arm wording ARE shared, and that module is where
// they live). Wasm-only for `pairing`'s reason: every seam it drives
// (`fauna-rpc-wasm`, the in-memory `MlsEngine`, `localStorage`) is wasm-only.
#[cfg(target_arch = "wasm32")]
pub mod succession;

// The member side of a succession — the witness the browser registers on its
// `FaunaMlsBackend`, its own wasm dialer, and the redrive seam its JS-ticked
// harvest pass re-drives parked statements through
// (`succession-propagation.md` § Propagation → *MLS groups*). Wasm-only for
// `succession`'s reason: `fauna-rpc-wasm`'s anonymous dial and the browser
// timer are wasm-only.
#[cfg(target_arch = "wasm32")]
mod succession_witness;

// The deployment-seed custody leg's two edges on web — the post-auth hook and
// the account runtime's store-ready edge, whichever lands second
// (`box-recovery.md` § The plane-era recovery floor, (c)). Wasm-only: it rides
// `fauna-rpc-wasm` and the account runtime's handle.
#[cfg(target_arch = "wasm32")]
mod deployment_seed_custody;

// Re-exports for per-app upload-sidecar wire-up. The web app (and any
// other wasm consumer) gets the audience-keyed seal pipeline + UploadSidecar
// serde via these without taking on the `process_media` feature's heavy
// native deps. Spec tracked internally.
pub use fauna_media::audience::{Audience, AudienceClass, RestrictedPostAudience};
pub use fauna_media::pipeline::{UploadPayload, process_and_seal};
pub use fauna_media::seal::{SealedBlob, seal_for_audience};
pub use fauna_media::sidecar::UploadSidecar;

// ── Upload sidecar (audience-keyed seal + UploadSidecar) ─────
//
// JS-callable wrappers over `fauna_media::process_and_seal`. The web app has
// two real blob-upload call-sites: the photo library (`Audience::Library`,
// sealed under the owner's `BackupKey`) and feed post attachments
// (`Audience::PublicPost`, plaintext passthrough). Each returns a
// `WasmUploadPayload` the caller POSTs as the `multipart/form-data` `sidecar` +
// `bytes` parts of `POST /api/v1/blob`. Spec tracked internally.
//
// The curated `process_media` (MIME sniff + EXIF/IPTC strip + JPEG thumbnail,
// no `c2pa-detect`) IS enabled for this bundle: `fauna-conversations`'
// wasm32 dependency block takes `fauna-media/process_media` for the
// conversation-attachment metadata strip, and Cargo unifies that feature onto
// every `fauna-media` consumer in the graph — this crate included. So a real
// image attached to a feed post is sniffed and thumbnailed in the browser, and
// both blobs cross the boundary (§ `thumbnail_bytes`). The assert below keeps
// that load-bearing: it is an inherited feature, not one this crate requests.
const _: () = assert!(
    fauna_media::process::PROCESS_MEDIA_ENABLED,
    "fauna-wasm needs fauna-media/process_media (inherited via fauna-conversations' \
     wasm32 block) — without it `process_and_seal` stamps no thumbnail_hash and the \
     feed producer silently degrades to full-size images"
);

/// The multipart-ready output of an audience-keyed seal: the DAG-CBOR
/// `UploadSidecar` bytes + the sealed (or, for `PublicPost`, plaintext) blob
/// bytes, plus the MIME the caller records on the referencing content — and,
/// when `process_media` derived one, the same pair for the thumbnail blob.
#[wasm_bindgen]
pub struct WasmUploadPayload {
    sidecar: Vec<u8>,
    bytes: Vec<u8>,
    mime: String,
    thumbnail_sidecar: Option<Vec<u8>>,
    thumbnail_bytes: Option<Vec<u8>>,
}

#[wasm_bindgen]
impl WasmUploadPayload {
    /// DAG-CBOR-encoded `UploadSidecar` — the `sidecar` multipart part.
    #[wasm_bindgen(getter)]
    pub fn sidecar(&self) -> Vec<u8> {
        self.sidecar.clone()
    }

    /// Sealed (Library) or plaintext (PublicPost) blob bytes — the `bytes` part.
    #[wasm_bindgen(getter)]
    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }

    /// MIME the caller stores on the referencing content's `MediaItem`.
    #[wasm_bindgen(getter)]
    pub fn mime(&self) -> String {
        self.mime.clone()
    }

    /// DAG-CBOR `UploadSidecar` for the thumbnail blob, or `undefined` when
    /// `process_media` derived no thumbnail (non-image, or already ≤300px).
    #[wasm_bindgen(getter, js_name = thumbnailSidecar)]
    pub fn thumbnail_sidecar(&self) -> Option<Vec<u8>> {
        self.thumbnail_sidecar.clone()
    }

    /// The thumbnail blob's bytes — the `bytes` part of its own
    /// `POST /api/v1/blob`. Present exactly when `thumbnailSidecar` is.
    ///
    /// The caller MUST POST this as a second blob **before** the primary: the
    /// primary's sidecar already declares `blake3` of these bytes as its
    /// `thumbnail_hash`, and the nest records that pointer when it ingests the
    /// primary. Dropping it leaves a sidecar routing at a blob that was never
    /// uploaded, which `?thumb=1` degrades by serving the full-size original.
    #[wasm_bindgen(getter, js_name = thumbnailBytes)]
    pub fn thumbnail_bytes(&self) -> Option<Vec<u8>> {
        self.thumbnail_bytes.clone()
    }
}

impl WasmUploadPayload {
    /// The already-flattened twin of [`from_payload`](Self::from_payload), for
    /// `WasmFeedManager::sealComposeAttachment` — the manager owns that seal
    /// (a tier's period key never crosses this boundary) and hands back parts,
    /// not an `UploadPayload`.
    ///
    /// ⚠ `mime` is the **plaintext's real** MIME here, deliberately not the
    /// sidecar's. This getter's contract is "the MIME the caller stores on the
    /// referencing content's `MediaItem`", and for an AEAD-sealed class the
    /// sidecar is pinned to `application/octet-stream` — so reading it off the
    /// sidecar, as [`from_payload`](Self::from_payload) correctly does for the
    /// plaintext classes, would type every gated photo as a byte blob.
    pub(crate) fn from_compose_attachment(a: fauna_feed::ComposeAttachmentUpload) -> Self {
        WasmUploadPayload {
            sidecar: a.primary.sidecar_cbor,
            bytes: a.primary.bytes,
            mime: a.media_type,
            thumbnail_sidecar: a.thumbnail.as_ref().map(|t| t.sidecar_cbor.clone()),
            thumbnail_bytes: a.thumbnail.map(|t| t.bytes),
        }
    }

    fn from_payload(p: UploadPayload) -> Self {
        let mime = p.primary_sidecar.mime.clone();
        // The shared flattening every app uses, so web's multipart bytes are
        // identical to native's for the same input (priority #1/#2).
        let (primary, thumbnail) = p.into_multipart_parts();
        WasmUploadPayload {
            sidecar: primary.sidecar_cbor,
            bytes: primary.bytes,
            mime,
            thumbnail_sidecar: thumbnail.as_ref().map(|t| t.sidecar_cbor.clone()),
            thumbnail_bytes: thumbnail.map(|t| t.bytes),
        }
    }
}

/// Seal owner-only library media under the owner's `BackupKey` (derived from the
/// identity seed) and build its `UploadSidecar`. `secret_hex` is the 64-char hex
/// Ed25519 seed.
#[wasm_bindgen]
pub fn process_and_seal_library(
    raw: &[u8],
    secret_hex: &str,
) -> Result<WasmUploadPayload, JsValue> {
    process_and_seal_library_inner(raw, secret_hex).map_err(|e| JsValue::from_str(&e))
}

/// Build the seal+sidecar payload for a public-post blob attachment. Bytes pass
/// through unsealed (the post's signature attests to the blob hash); `mime` is
/// the browser-supplied Content-Type to serve on download (the stub
/// `process_media` can't sniff it). `has_c2pa` is the browser's own C2PA
/// manifest detection over the raw bytes (the SDK the viewer path already
/// ships) — the stub `process_media` can't detect it either
/// (`media.md` § C2PA provenance, *Upload-side `has_c2pa` population on web*).
#[wasm_bindgen]
pub fn process_and_seal_public_post(raw: &[u8], mime: &str, has_c2pa: bool) -> WasmUploadPayload {
    process_and_seal_public_post_inner(raw, mime, has_c2pa)
}

/// The DAG-CBOR `UploadSidecar` bytes for a gated post's **already-sealed**
/// full-body blob — the web twin of the sidecar the native
/// `fauna_client::upload_gated_post_blob` builds, so every app ships a
/// byte-identical `PeriodRestrictedPost` sidecar (priority #1/#2). The sealed
/// bytes come from `WasmFeedManager::prepareGatedBlob` (sealed by the shared
/// post builder); this is transport glue only — no `process_and_seal` (the real
/// MIME rides inside the seal, so the sidecar mime is `application/octet-stream`
/// and there is no thumbnail). The SPA POSTs these bytes as the `multipart`
/// `sidecar` part alongside the sealed blob as `bytes`
/// (`ui/feed.md` § Encryption at rest).
#[wasm_bindgen]
pub fn gated_post_sidecar() -> Vec<u8> {
    UploadSidecar::gated_post().to_dag_cbor()
}

pub fn process_and_seal_library_inner(
    raw: &[u8],
    secret_hex: &str,
) -> Result<WasmUploadPayload, String> {
    let bytes = hex::decode(secret_hex.trim()).map_err(|e| format!("bad secret hex: {e}"))?;
    let seed: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "secret must be 64 hex chars".to_string())?;
    let audience = Audience::Library {
        backup_key: fauna_core::crypto::BackupKey::derive(&seed),
    };
    Ok(WasmUploadPayload::from_payload(process_and_seal(
        raw, &audience,
    )))
}

pub fn process_and_seal_public_post_inner(
    raw: &[u8],
    mime: &str,
    has_c2pa: bool,
) -> WasmUploadPayload {
    // `post_id` is unused by the PublicPost seal (no key derivation) and the
    // blob uploads before the post that references it exists, so a zero
    // placeholder is the honest value — the binding lives on the post's
    // reference list, not the sidecar (spec § Sidecar: what it carries and why).
    let audience = Audience::PublicPost {
        post_id: ContentHash::from_digest_raw([0u8; 32]),
    };
    let mut payload = process_and_seal(raw, &audience);
    // PublicPost serves bytes verbatim with the sidecar `mime` as the
    // Content-Type, and the per-class verifier requires a non-empty
    // type/subtype. The sniffed MIME wins whenever `process_media` recognized
    // the format — that is the value every native app declares for the same
    // bytes (`fauna_client::upload_public_post_blob`), and it can't be spoofed
    // by a wrong browser `File.type`. Only when sniffing declines (a format
    // outside its table, e.g. SVG) do we fall back to the browser's
    // Content-Type, which is real information native callers simply don't have.
    if payload.primary_sidecar.mime == "application/octet-stream" && !mime.trim().is_empty() {
        payload.primary_sidecar.mime = mime.to_string();
    }
    // One-way OR: the browser's own manifest parse can only ADD provenance the
    // stub `process_media` missed, never override a future real in-wasm
    // detection. Gated on the FINAL sniffed mime (mirrors native's
    // `mime.starts_with("image/")` gate, `fauna-media/src/process.rs:76`) so a
    // browser false-positive on a non-image blob can't set the flag.
    if has_c2pa && payload.primary_sidecar.mime.starts_with("image/") {
        payload.primary_sidecar.has_c2pa = true;
    }
    WasmUploadPayload::from_payload(payload)
}

// ── WASM exports (thin wrappers) ─────────────────────────────

#[wasm_bindgen]
pub fn generate_keypair() -> String {
    generate_keypair_inner()
}

#[wasm_bindgen]
pub fn actor_id_from_secret(secret_hex: &str) -> Result<String, JsValue> {
    actor_id_from_secret_inner(secret_hex).map_err(|e| JsValue::from_str(&e))
}

/// Encode a secret into a `fauna://identity?secret=…[&handle=…]` URI; when `handle` is
/// present (and non-empty) the QR carries the `(identity, handle)` payload so a scanned
/// import pre-fills the handle step (onboarding.md §1.identity_import).
#[wasm_bindgen]
pub fn identity_qr_encode(secret_hex: &str, handle: Option<String>) -> String {
    fauna_core::identity_qr::IdentityQr::to_uri(secret_hex, handle.as_deref())
}

#[wasm_bindgen]
pub fn identity_qr_decode(uri: &str) -> Result<String, JsValue> {
    fauna_core::identity_qr::IdentityQr::from_uri(uri).map_err(|e| JsValue::from_str(&e))
}

/// `fauna_core::identity_qr::parse_import_input` → a flat `[secret, handle]` array (handle
/// `""` when absent) classifying a pasted/scanned identity-import field — a bare 64-hex
/// secret, the `fauna://identity?secret=&handle=` query form, or the iOS colon form. An
/// **empty array** signals a parse failure. Web's onboarding shares this with iOS/android
/// instead of hand-rolling the input grammar (priority #2/#4).
/// Register the SPA's aftermath progress callback for **leg 3**, the `__mls`
/// re-seal (`succession-aftermath.md` § Re-key scope, the `BackupKey` corpus
/// row).
///
/// Free rather than a method on a client or the conversations manager because
/// of *when* it must run: leg 3 reports from inside the conversations replica's
/// own `load()`, so the sink has to be in place **before** that plane is built,
/// which is earlier than either object exists. The SPA calls this once, ahead
/// of `conversationsManager(..)`.
///
/// Takes the same `(leg, line | null) => void` callback
/// `runSuccessionAftermath` takes, and files under `mlsReseal`, so the SPA has
/// one progress channel for all seven legs rather than two shapes to reconcile.
#[wasm_bindgen(js_name = setMlsResealSink)]
pub fn set_mls_reseal_sink(on_progress: Option<js_sys::Function>) {
    crate::succession::set_mls_reseal_sink(on_progress);
}

#[wasm_bindgen(js_name = parseIdentityImport)]
pub fn parse_identity_import(input: &str) -> Vec<String> {
    fauna_core::identity_qr::parse_import_input(input)
        .map(|i| i.into_parts())
        .unwrap_or_default()
}

/// `fauna_core::qr_matrix::qr_matrix` → `{ size, modules }`: a square, row-major grid of
/// dark/light flags (`modules[y * size + x]`) the SPA paints itself, rather than pulling a
/// JS QR library — the same encoder the five native apps call over UniFFI
/// (`settings.md` § Identity export; priorities #1/#2).
///
/// The grid carries **no quiet zone**: pad by [`qr_quiet_zone_modules`] on all four sides
/// when drawing, or scanners refuse the code. Deliberately generic over the payload — the
/// identity export composes `identityQrEncode` → `qrMatrix`.
///
/// Rejects a payload past the largest QR version's capacity at EC level M.
#[wasm_bindgen(js_name = qrMatrix)]
pub fn qr_matrix(data: &str) -> Result<JsValue, JsValue> {
    let m = fauna_core::qr_matrix::qr_matrix(data).map_err(|e| JsValue::from_str(&e))?;
    crate::rpc::to_js(&m)
}

/// The quiet-zone margin, in modules, a renderer must leave around [`qr_matrix`]'s grid —
/// the shared `fauna_core::qr_matrix::QUIET_ZONE_MODULES`, so the SPA doesn't hard-code
/// its own `4`.
#[wasm_bindgen(js_name = qrQuietZoneModules)]
pub fn qr_quiet_zone_modules() -> u32 {
    fauna_core::qr_matrix::QUIET_ZONE_MODULES
}

/// `fauna_core::format::byte_size` → a `LocalizedText` `{ key, args }` the SPA
/// resolves through `L()`. See `docs/goal/behavior/value-formatting.md`.
#[wasm_bindgen(js_name = byteSize)]
pub fn byte_size(bytes: f64) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::byte_size(bytes as u64))
}

/// `fauna_core::format::relative_time_display` → `{ localized?: {key,args},
/// absolute_epoch_ms?: number }`: render `localized` via `L()` for recent items,
/// else format `absolute_epoch_ms` with a native date formatter (≥7d old).
#[wasm_bindgen(js_name = relativeTime)]
pub fn relative_time(now_ms: f64, then_ms: f64) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::relative_time_display(
        now_ms as i64,
        then_ms as i64,
    ))
}

/// `fauna_core::format::grace_countdown` → `LocalizedText | undefined`: `Some`
/// (`{key: "time.countdown_dh"|"time.countdown_h", args}`) while `deadlineMs >
/// nowMs`, resolved via `L()`; `undefined` once elapsed — the caller renders its
/// own already-localized "elapsed" label. See value-formatting.md § Grace
/// countdown.
#[wasm_bindgen(js_name = graceCountdown)]
pub fn grace_countdown(deadline_ms: f64, now_ms: f64) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::grace_countdown(
        deadline_ms as i64,
        now_ms as i64,
    ))
}

/// `fauna_core::format::duration_secs` → a `LocalizedText` `{ key, args }` the SPA
/// resolves through `L()` (coarse uptime/duration). See value-formatting.md.
#[wasm_bindgen(js_name = durationSecs)]
pub fn duration_secs(secs: f64) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::duration_secs(secs as u64))
}

/// `fauna_core::format::tip_amount` → a `LocalizedText` `{ key, args }`
/// resolved through `L()`. See `monetization.md` § Tips.
// `payments`-gated doc line — see § Gated element ids live in gated DOC LINES.
#[cfg_attr(feature = "payments", doc = " Rendered into `post-tip-total`.")]
#[wasm_bindgen(js_name = tipAmount)]
pub fn tip_amount(msats: f64) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::tip_amount(msats as i64))
}

/// `fauna_core::format::tip_count` → a `LocalizedText` `{ key, args }`
/// resolved through `L()`. See `monetization.md` § Tips.
// `payments`-gated doc line — see § Gated element ids live in gated DOC LINES.
#[cfg_attr(feature = "payments", doc = " Rendered into `post-tip-count`.")]
#[wasm_bindgen(js_name = tipCount)]
pub fn tip_count(count: f64) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::tip_count(count as i64))
}

/// `fauna_core::format::tip_more` → a `LocalizedText` `{ key, args }` for the
/// "and N more" tail, resolved through `L()`. See `monetization.md` § Tips.
// `payments`-gated doc line — see § Gated element ids live in gated DOC LINES.
#[cfg_attr(feature = "payments", doc = " The tail belongs to `post-tip-list`.")]
#[wasm_bindgen(js_name = tipMore)]
pub fn tip_more(n: f64) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::tip_more(n as i64))
}

/// `fauna_core::format::conversation_timestamp_display` → `{ clock?: string,
/// localized?: {key,args}, absolute_epoch_ms?: number }` (exactly one set) for a
/// thread's last-activity time, bucketed in the caller's local timezone
/// (`utcOffsetSeconds`): render `clock` for today, `localized` via `L()` for
/// Yesterday / a weekday, else format `absolute_epoch_ms` with a native date
/// formatter. See value-formatting.md § Conversation timestamp.
#[wasm_bindgen(js_name = conversationTimestamp)]
pub fn conversation_timestamp(
    now_ms: f64,
    then_ms: f64,
    utc_offset_seconds: i32,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::conversation_timestamp_display(
        now_ms as i64,
        then_ms as i64,
        utc_offset_seconds,
    ))
}

/// `fauna_core::format::parse_cap` → parse an admin tier-cap field
/// (`admin-settings-tier-cap-*`) to the non-negative cap, or `undefined` for
/// empty / non-numeric / fractional / overflowing input. A negative value clamps
/// to `0`. Returned as `f64` (web's `number`, matching the 64-bit wasm convention
/// of `byteSize`/`relativeTime`); the cap stays well within `Number.MAX_SAFE_INTEGER`.
/// Web's `admin/settings/+page.svelte` consumes it as `parseCap(text) ?? prev`, so
/// a blank/unparseable edit keeps the persisted cap. See value-formatting.md
/// § Tier cap validation.
#[wasm_bindgen(js_name = parseCap)]
pub fn parse_cap(input: &str) -> Option<f64> {
    fauna_core::format::parse_cap(input).map(|v| v as f64)
}

/// `fauna_core::format::parse_count` → parse an admin-mail integer knob
/// (`admin-mail-*-input`) to the non-negative `u32`, or `undefined` for empty /
/// non-numeric / negative / fractional / overflowing input. A leading `+` is
/// accepted (the canonical rule — web's prior `^\d+$` rejected it, a deliberate
/// convergence). Returned as `f64` (web's `number`). Web's
/// `admin/mail/+page.svelte` consumes it as `parseCount(text) ?? prev` on a
/// full-PUT save, so a blank/unparseable edit keeps the persisted knob. See
/// value-formatting.md § Mail-knob validation.
#[wasm_bindgen(js_name = parseCount)]
pub fn parse_count(input: &str) -> Option<f64> {
    fauna_core::format::parse_count(input).map(|v| v as f64)
}

/// `fauna_core::format::parse_count_u64` → the `u64` sibling of [`parse_count`]
/// for the one mail knob whose range can exceed `u32` (the IMAP per-mailbox
/// storage ceiling, `admin-mail-imap-storage-bytes-input`). Same semantics;
/// returned as `f64` (byte ceilings stay within `Number.MAX_SAFE_INTEGER`).
#[wasm_bindgen(js_name = parseCountU64)]
pub fn parse_count_u64(input: &str) -> Option<f64> {
    fauna_core::format::parse_count_u64(input).map(|v| v as f64)
}

/// `fauna_core::format::parse_count_i64` → the signed-`i64` sibling of
/// [`parse_count`] for the one **per-alias** knob whose wire column is signed
/// `i64`: the mail-alias `rate_limit_per_hour` override
/// (`mail-aliases-add-sheet-rate-per-hour-input` → `rate_limit_per_hour:
/// Option<i64>`), parsed as a non-negative cap. Empty / non-numeric / negative /
/// fractional input → `undefined` = "no override" (NOT a fall-back-to-prev,
/// unlike the admin-mail knobs). Returned as `f64` (web's `number`). See
/// value-formatting.md § Mail-knob validation.
#[wasm_bindgen(js_name = parseCountI64)]
pub fn parse_count_i64(input: &str) -> Option<f64> {
    fauna_core::format::parse_count_i64(input).map(|v| v as f64)
}

/// `fauna_core::format::parse_port` → a validated TCP port (`1..=65535`) parsed
/// from a user-entered admin field (CalDAV/serving-port), or `undefined` for
/// empty / non-numeric / negative / fractional / out-of-range input — notably
/// `0`, which is not a bindable listener port. Returned as `f64` (web's
/// `number`; ports stay well within `Number.MAX_SAFE_INTEGER`). Web's
/// `admin-calendar`/`admin-nest` pages consume it in place of their local
/// `Number.isInteger(..)` range check (already the pattern android/apple/windows
/// use via `fauna_core::format::parse_port`'s FFI export). See
/// value-formatting.md § Port validation.
#[wasm_bindgen(js_name = parsePort)]
pub fn parse_port(input: &str) -> Option<f64> {
    fauna_core::format::parse_port(input).map(|v| v as f64)
}

/// `fauna_client_admin::parse_region_code` → a validated declared-region code
/// (`admin-nest-region-input`'s client-side refusal path,
/// `test_malformed_region_is_refused_client_side`), or `undefined` for a
/// malformed code — 2-8 characters, each an uppercase ASCII letter or digit,
/// never case-folded (two spellings of one region must not both be
/// storable). The `Err` this discards carries the shared engine's own i18n
/// KEY, not a display string — every caller renders its own local
/// `region_invalid` message instead, the same discipline `parsePort` uses.
/// DECLARED, NEVER DETECTED (ratified 2026-08-11): no caller may add a
/// detect/prefill affordance around this.
#[wasm_bindgen(js_name = adminParseRegionCode)]
pub fn admin_parse_region_code(input: &str) -> Option<String> {
    fauna_client_admin::parse_region_code(input)
        .ok()
        .map(|code| code.as_str().to_string())
}

/// `fauna_core::format::parse_weight_permille` → the create-feed factor-weight
/// editor's decimal multiplier (`feed-factor-weight-input`, e.g. `"2.0"`) → the
/// wire's signed per-mille `FactorWeightInput.weight_permille`
/// (content-moderation-and-ranking.md § Composition). Unparseable / non-finite
/// input → the `1.0` baseline (`1000`); a **negative** weight is a designed case
/// (a strong-negative factor sinks an item). Replaces web's
/// `Math.round(Number.parseFloat(..) * 1000)`, which drifted twice from the
/// canonical rule: `parseFloat` is lenient (`"2abc"` → `2000`) and JS `Math.round`
/// is half-**up** (`-0.0025` → `-2`, where the shared fn yields `-3`). Returned as
/// `f64` (web's `number`). See value-formatting.md § Factor weight.
#[wasm_bindgen(js_name = parseWeightPermille)]
pub fn parse_weight_permille(input: &str) -> f64 {
    fauna_core::format::parse_weight_permille(input) as f64
}

/// `fauna_core::format::format_weight_permille` — the inverse of
/// [`parse_weight_permille`]: a wire `weight_permille` → the create-feed factor
/// chip's display multiplier (`"{name} × {weight}"`). Rounds to 2 decimal places
/// and strips trailing zeros, so a whole multiplier reads `"1"`, never `"1.00"`.
/// Takes `f64` (web's `number`) since `weight_permille` crosses the wire as a
/// JSON number, not `bigint`. See value-formatting.md § Factor weight.
#[wasm_bindgen(js_name = formatWeightPermille)]
pub fn format_weight_permille(weight_permille: f64) -> String {
    fauna_core::format::format_weight_permille(weight_permille as i64)
}

/// `fauna_core::format::backup_destination_label` → the backup-destination row
/// label: the `display_name` when set (non-empty), else the destination URL's
/// host (scheme/port/path stripped). A plain `string`, so web stops hand-rolling
/// the `split('://')` host parse in `destLabel`. See backups.md § State & data shape.
#[wasm_bindgen(js_name = backupDestinationLabel)]
pub fn backup_destination_label(
    display_name: Option<String>,
    destination_nest_url: &str,
) -> String {
    fauna_core::format::backup_destination_label(display_name.as_deref(), destination_nest_url)
}

/// `fauna_core::format::backup_last_upload_label` → the
/// `backup-destination-last-upload-time` row text `{ label: {key,args},
/// when?: { localized?: {key,args}, absolute_epoch_ms?: number } }`: resolve
/// `label` via `L()`; when `when` is present, resolve it first (same shape as
/// [`relative_time`]) and substitute it as the label's `{when}` arg.
/// `last_upload_secs` is the status's raw `last_upload_time` (unix **seconds**;
/// `undefined`/`0` ⇒ "never" — the shared guard). One source of truth for the
/// never-vs-real decision + seconds→ms conversion web's `lastUploadText`
/// hand-rolled. See value-formatting.md § Backup destination status labels.
#[wasm_bindgen(js_name = backupLastUploadLabel)]
pub fn backup_last_upload_label(
    last_upload_secs: Option<f64>,
    now_ms: f64,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::backup_last_upload_label(
        last_upload_secs.map(|s| s as u64),
        now_ms as i64,
    ))
}

/// `fauna_core::format::backup_backlog_label` → a complete `LocalizedText`
/// `{ key, args }` (`backups.backup_destination_backlog` + `{count}`) for the
/// `backup-destination-backlog-count` row text; `undefined` (no status read
/// yet) carries the shared 0 baseline. See value-formatting.md § Backup
/// destination status labels.
#[wasm_bindgen(js_name = backupBacklogLabel)]
pub fn backup_backlog_label(backlog_count: Option<u32>) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::backup_backlog_label(backlog_count))
}

/// `fauna_core::format::backup_last_audit_label` → the
/// `backup-destination-last-audit-time` row text, same
/// `{ label, when? }` shape as [`backup_last_upload_label`] and resolved the same
/// way. `last_passed_secs` is the record's `state.last_passed_at` (unix
/// **seconds**; `undefined`/`0` ⇒ "never").
///
/// **A deliberately separate row from last-*upload*.** The upload row is the
/// *source nest* reporting on its own work; this row is what the *client's own*
/// audit independently confirmed — a client rendering one where it meant the
/// other is precisely the confusion the audit exists to prevent
/// (`fauna_core::format::BackupLastAuditDisplay`). See `backups.md`
/// § Audit-alert surface and value-formatting.md § Backup destination status labels.
#[wasm_bindgen(js_name = backupLastAuditLabel)]
pub fn backup_last_audit_label(
    last_passed_secs: Option<f64>,
    now_ms: f64,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::backup_last_audit_label(
        last_passed_secs.map(|s| s as u64),
        now_ms as i64,
    ))
}

/// `fauna_core::format::backup_self_audit_label` → the
/// `backup-destination-last-audit-time` row text for a **client-device
/// custodian** row, same `{ label, when? }` shape as [`backup_last_audit_label`]
/// and resolved the same way. A separate door from that one all the way down:
/// the owner-side loop and a custodian's self-report answer the same question
/// from opposite sides of the trust line (`docs/goal/ui/backups.md`
/// § Audit-alert surface → *The client-device arm*).
///
/// `last_passed_secs` is the status row's `last_audit_passed_at` (unix
/// **seconds**; `undefined`/`0` ⇒ "Self-checked: not yet", never a verdict).
#[wasm_bindgen(js_name = backupSelfAuditLabel)]
pub fn backup_self_audit_label(
    last_passed_secs: Option<f64>,
    now_ms: f64,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::backup_self_audit_label(
        last_passed_secs.map(|s| s as u64),
        now_ms as i64,
    ))
}

/// `fauna_core::format::backup_self_audit_is_alerting` → whether a custodian's
/// reported `audit_state` (the status row's own field) must raise a
/// `backup-audit-alert` with `BackupAuditAlertReason::SelfReported`. Absence
/// and an unrecognised value both stay quiet — the single shared answer, so no
/// app can drift into alerting on a custodian that has simply not audited yet.
#[wasm_bindgen(js_name = backupSelfAuditIsAlerting)]
pub fn backup_self_audit_is_alerting(audit_state: Option<String>) -> bool {
    fauna_core::format::backup_self_audit_is_alerting(audit_state.as_deref())
}

/// The opaque `BackupAuditAlertReason::SelfReported` value, for a
/// [`backup_self_audit_is_alerting`]-flagged client-device row's
/// `backup-audit-alert` banner. The SPA never constructs a reason by hand —
/// every other arm arrives pre-built on an audit row — so this is the one
/// door that hands it one, keeping `BackupAuditAlertReason` opaque (`unknown`)
/// to TypeScript exactly like every other arm.
#[wasm_bindgen(js_name = backupSelfReportedAlertReason)]
pub fn backup_self_reported_alert_reason() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::BackupAuditAlertReason::SelfReported)
}

/// `fauna_core::format::backup_audit_alert_label` → the complete `LocalizedText`
/// `{ key, args }` for one `backup-audit-alert` banner, naming **both** the
/// destination and the reason.
///
/// `reason` is a `BackupAuditAlertReason` handed straight back from the
/// `alert_reason` field of a `backupAuditRunPass` row — the SPA never inspects or
/// constructs it. That is the point: which verdicts are loud, and what each one
/// says, both stay in shared Rust
/// (`DestinationAuditRecord::alert_reason`/`AuditVerdict::is_alerting`), so no
/// client can drift into alerting on a transient `Unreachable` — the
/// laptop-on-a-plane case the loop keeps quiet by design.
#[wasm_bindgen(js_name = backupAuditAlertLabel)]
pub fn backup_audit_alert_label(
    reason: JsValue,
    destination_label: String,
) -> Result<JsValue, JsValue> {
    let reason: fauna_core::format::BackupAuditAlertReason = crate::rpc::from_js(reason)?;
    crate::rpc::to_js(&fauna_core::format::backup_audit_alert_label(
        reason,
        &destination_label,
    ))
}

/// `fauna_core::format::backup_destination_kind_label` → the
/// `backup-destination-kind-badge` text `{ key, args }` for one destination row.
///
/// `kind` is the row's raw `kind` discriminator, handed straight back from
/// `backupDestinationList`. **An unrecognised kind renders as itself** (the raw
/// string interpolated) rather than collapsing into a generic word: a row a
/// newer client wrote is precisely the case where the user needs to see *what*
/// their older build cannot drive. See backups.md § Third destination kind.
#[wasm_bindgen(js_name = backupDestinationKindLabel)]
pub fn backup_destination_kind_label(kind: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::backup_destination_kind_label(kind))
}

/// `fauna_core::format::backup_destination_kind_options` → the
/// `backup-destination-kind-select` catalog as `[{ value, label: {key, args} }]`
/// — the implemented kinds in paint order (nest first, the kind that actually
/// satisfies "off-site").
///
/// The property the catalog exists for: **the option a user picks and the badge
/// they get back must be the same text**, so the SPA must not pair a
/// hand-written `<option>` list against [`backup_destination_kind_label`]. The
/// ratified-but-deferred S3 kind is absent rather than present-and-disabled.
#[wasm_bindgen(js_name = backupDestinationKindOptions)]
pub fn backup_destination_kind_options() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::backup_destination_kind_options())
}

/// `fauna_core::format::publish_kind_options` → the
/// `personalization-trained-factor-publish-kind-select` catalog as
/// `[{ value, label: {key, args} }]`, in paint order (List first — the weaker
/// disclosure, so it is what an unattended default picks). RAW-VALUE: the
/// `<option value>` the SPA renders is `value` itself, never the resolved
/// label — the option a user picks and the words they read back at
/// `labeler-catalog-item-kind` must be the same text.
#[wasm_bindgen(js_name = publishKindOptions)]
pub fn publish_kind_options() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::publish_kind_options())
}

/// `fauna_core::format::publish_kind_label` → the kind-select option's
/// `{key, args}` text for one wire value. Paint-only: the select round-trips
/// the wire discriminator, so this never becomes a driver contract.
#[wasm_bindgen(js_name = publishKindLabel)]
pub fn publish_kind_label(kind: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::publish_kind_label(kind))
}

/// `fauna_core::format::ngram_direction_label` → the Model review row's
/// class-direction text (`personalization-trained-factor-publish-ngram-direction`,
/// and the `labeler-inspect-model-entry-direction` twin) — the same shared
/// face both surfaces read, so a publisher's review cannot disagree with what
/// a subscriber later sees.
#[wasm_bindgen(js_name = ngramDirectionLabel)]
pub fn ngram_direction_label(more: u32, less: u32) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::ngram_direction_label(more, less))
}

/// `fauna_core::format::ngram_doc_count_label` → the Model review row's
/// class-blind distinct-document count (`…-publish-ngram-count` /
/// `labeler-inspect-model-entry-count`) — `more + less`, the quantity the
/// 3-post privacy floor bounds.
#[wasm_bindgen(js_name = ngramDocCountLabel)]
pub fn ngram_doc_count_label(more: u32, less: u32) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::ngram_doc_count_label(more, less))
}

/// `fauna_core::format::text_model_needs_newer_app` → the
/// `labeler-catalog-item-kind` badge's override text, or `null` when the row
/// should paint its raw `artifact_kind` discriminator unchanged (every
/// ordinary row). The one non-passthrough field: a subscribed `text-model`
/// whose tokenizer contract this build does not implement says so here, over
/// the same predicate the compose seam's inert branch reads
/// (`scoring::text_model_version_supported`), so the badge cannot disagree
/// with the scorer.
#[wasm_bindgen(js_name = textModelNeedsNewerApp)]
pub fn text_model_needs_newer_app(
    artifact_kind: &str,
    artifact_version: f64,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::text_model_needs_newer_app(
        artifact_kind,
        artifact_version as u64,
    ))
}

/// `fauna_core::format::backup_usage_label` → the `backup-destination-usage`
/// row text `{ label: {key,args}, held?: {key,args}, cap?: {key,args} }`
/// (client-device rows only): held bytes against the user-set cap.
///
/// Resolve `label` via `L()`; when `held` / `cap` are present resolve each first
/// (they are themselves byte-size `LocalizedText`s) and substitute them as the
/// label's `{held}` / `{cap}` args — the same two-level shape as
/// [`backup_last_upload_label`], and for the same reason.
///
/// ⚠ **`capState` is read, never inferred.** Pass the status row's `cap_state`
/// through; do **not** re-derive cap-reached from `held >= cap`. A pull pass
/// that stopped at its cap ends *below* the cap (a segment larger than the
/// remaining headroom stops the pass without filling it), so inferring the
/// verdict from the two numbers renders "healthy, with room to spare" for a
/// backup that has silently stopped advancing.
///
/// `heldBytes: undefined` is "this custodian has never checked in", which reads
/// as *nothing held yet* rather than *0 bytes held*.
#[wasm_bindgen(js_name = backupUsageLabel)]
pub fn backup_usage_label(
    held_bytes: Option<f64>,
    capacity_cap_bytes: Option<f64>,
    cap_state: Option<String>,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::backup_usage_label(
        held_bytes.map(|b| b as u64),
        capacity_cap_bytes.map(|b| b as u64),
        cap_state.as_deref(),
    ))
}

/// `fauna_core::format::parse_byte_size` → the inverse of [`byte_size`], and the
/// one the `backup-destination-capacity-input` cap is read through on every app.
///
/// Liberal about what a person types (`"50 GB"`, `"50GB"`, `"1,5 TB"`, a bare
/// `"1024"`) and strict about what counts as a number; units are **1024-based**,
/// matching [`byte_size`]'s own scaling, so a cap round-trips through the two
/// unchanged instead of drifting every time the page repaints it.
///
/// `undefined` is a **refusal the SPA must surface on `error-message`**, never a
/// substituted default: silently recording a cap the user did not choose is
/// exactly the class of guess that fills a device's disk.
///
/// Returns a JS `number` (not a `bigint`) so it feeds straight back into
/// [`byte_size`], which takes one — every value a storage cap can hold is exact
/// in a double.
#[wasm_bindgen(js_name = parseByteSize)]
pub fn parse_byte_size(input: &str) -> Option<f64> {
    fauna_core::format::parse_byte_size(input).map(|b| b as f64)
}

/// `fauna_core::data::every_row_is_a_client_device` → does every configured
/// destination hold its copy on one of the owner's own devices? The
/// `backup-sole-client-destination-warning` predicate (backups.md § Third
/// destination kind → *Durability + labeling*).
///
/// `destinations` is the array `backupDestinationList` returned, handed straight
/// back — the same "pass back the rows you already have" shape the UniFFI twin
/// (`every_destination_is_a_client_device`) takes. A value that is not a destination list
/// **rejects** rather than answering: the wrong answer here is silent (the user
/// simply never sees a warning about durability they do not have).
///
/// Shared because it is a **policy** answer, not a rendering one. Both arms are
/// the conservative direction and neither is guessable — an empty list is *not*
/// sole-client (painting a durability warning on an account with no backup at
/// all is simply false), and a row whose kind this build does not implement
/// counts as *not* a client device (it may well BE the off-site copy the warning
/// would otherwise deny the user has).
#[wasm_bindgen(js_name = everyDestinationIsAClientDevice)]
pub fn every_destination_is_a_client_device(destinations: JsValue) -> Result<bool, JsValue> {
    let rows: Vec<fauna_core::data::BackupDestination> = crate::rpc::from_js(destinations)?;
    Ok(fauna_core::data::every_destination_is_a_client_device(
        &rows,
    ))
}

/// `fauna_client_feed::rule_type_options` → the create-feed rule-builder's
/// `feed-rule-type-select` catalog as `[{ value, label: {key, args}, input_kind }]`
/// — the 11 wire values ui.yaml pins, each with its `feed.rule_types.*` label and
/// the input widget its row shows. The SPA binds the select to `value`, resolves
/// `label` through `L()`, and switches its value/toggle inputs on `input_kind`
/// instead of the 7-branch `{#if}` ladder it re-derived. The web twin of the FFI
/// `rule_type_options`. See `docs/goal/ui/feed.md` § Where logic lives.
#[wasm_bindgen(js_name = ruleTypeOptions)]
pub fn rule_type_options() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_feed::rule_type_options())
}

/// `fauna_client_feed::builtin_factor_options` → the built-in head of the
/// create-feed `feed-factor-select` list as `[{ value, label: {key, args} }]`
/// (`engagement`, `trending`). The SPA offers these first, then the caller's
/// subscribed labeler and trained-topic factors, instead of its own
/// `'engagement'` literal. The web twin of the FFI `builtin_factor_options`.
#[wasm_bindgen(js_name = builtinFactorOptions)]
pub fn builtin_factor_options() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_feed::builtin_factor_options())
}

/// `fauna_client_feed::rule_required_label` → a `LocalizedText` `{ key, args }`
/// for the `feed-rule-required-toggle` — "Required" when the boolean rule demands
/// the trait, "Excluded" when it forbids it (`ui.yaml:5333-5334`). Web renders a
/// static "Required" today, which reads as the opposite of the rule the user built
/// (the nest evaluates `required:false` as a genuine exclusion). The web twin of
/// the FFI `rule_required_label`.
#[wasm_bindgen(js_name = ruleRequiredLabel)]
pub fn rule_required_label(required: bool) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_feed::rule_required_label(required))
}

/// `fauna_client_feed::rule_summary_label` → a `LocalizedText` `{ key, args }`
/// for one added-rule chip — `feed.md:171-183`'s *Example display* prose
/// (`#rust, #fauna`, `media: yes`, `replies >= 5`). This is web's own `ruleLabel`
/// prose lifted: the goal doc documented the web builder as the reference, so the
/// SPA keeps its exact wording while the four natives stop rendering raw
/// PascalCase wire keys — and web's copy stops being hard-coded English. The web
/// twin of the FFI `rule_summary_label`.
#[wasm_bindgen(js_name = ruleSummaryLabel)]
pub fn rule_summary_label(
    rule_type: &str,
    value: &str,
    required: bool,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_feed::rule_summary_label(
        rule_type, value, required,
    ))
}

/// `fauna_client_feed::can_add_rule` → whether the staged
/// `(input_kind, value, threshold)` create-feed rule inputs are complete
/// enough to enable `feed-add-rule-button` (`docs/goal/ui/feed.md` § Add-rule
/// gating) — apple's `FeedCreateForm.canAddRule`, lifted. `input_kind` is the
/// same string a `ruleTypeOptions()` row's `input_kind` field already carries
/// (`"Text"` / `"Number"` / `"Toggle"` / `"TextAndNumber"`), so the SPA passes
/// its existing `newRuleInputKind` straight through with no re-derivation.
#[wasm_bindgen(js_name = canAddRule)]
pub fn can_add_rule(input_kind: &str, value: &str, threshold: &str) -> Result<bool, JsValue> {
    let kind = match input_kind {
        "Text" => fauna_client_feed::RuleInputKind::Text,
        "Number" => fauna_client_feed::RuleInputKind::Number,
        "Toggle" => fauna_client_feed::RuleInputKind::Toggle,
        "TextAndNumber" => fauna_client_feed::RuleInputKind::TextAndNumber,
        other => {
            return Err(JsValue::from_str(&format!(
                "unknown rule input kind: {other}"
            )));
        }
    };
    Ok(fauna_client_feed::can_add_rule(kind, value, threshold))
}

/// `fauna_core::scoring::muted_keywords_collapse` → does a decrypted
/// conversation `body` collapse behind the user's `muted_keywords` list — the
/// `keywords` of the muted-words page record, `[{ keyword, weight }]`? Only a
/// term muted at the full penalty collapses (a softer weight only demotes in a
/// ranked feed). The web twin of the FFI `matches_muted_keywords`, so web and
/// native share one collapse-decision definition
/// (content-moderation-and-ranking.md § Composition). The conversation view
/// calls it post-decrypt to collapse a message behind the muted-keyword reveal
/// affordance; `false` for an empty list.
#[wasm_bindgen(js_name = matchesMutedKeywords)]
pub fn matches_muted_keywords(body: &str, muted_keywords: JsValue) -> Result<bool, JsValue> {
    let muted: Vec<fauna_core::data::MutedKeyword> = crate::rpc::from_js(muted_keywords)?;
    Ok(fauna_core::scoring::muted_keywords_collapse(&muted, body))
}

/// `fauna_core::scoring::MutedKeywordLevel::of` → the level the muted-words
/// page's level picker shows for a row's stored `weight`: `"hide"` iff the
/// weight collapses (the full penalty), else `"show-less"`. The web twin of
/// the FFI `muted_keyword_level`, so every app classifies a row by the one
/// shared threshold rather than comparing numbers of its own. `weight` is a
/// JS number (the record's `i64` as the page record carries it).
#[wasm_bindgen(js_name = mutedKeywordLevel)]
pub fn muted_keyword_level(weight: f64) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::scoring::MutedKeywordLevel::of(weight as i64))
}

/// `fauna_protocol::offline_class::affordance` → `{ available: boolean, reason?:
/// LocalizedText }` — W4 (account-data-plane.md § Workstreams) phase 4's UI-desensitizing decision, the ONE rule that
/// says whether a surface may offer an affordance right now
/// (`account-data-plane.md` § The offline-mutation contract → *How a surface
/// asks*). `kind` is the wire kind the gesture issues; `connection_state` is the
/// same lowercase word `connectionStateLabel` takes, so the SPA's
/// `connection-status` indicator and its gate cannot disagree about what
/// "connected" means.
///
/// The SPA calls this instead of testing a class itself: the rule makes three
/// rulings that are easy to get subtly backwards (only class 3 desensitizes; an
/// unregistered kind stays *available*; only the *known* offline words count as
/// offline), and `reason` is per affordance — the charter forbids expressing it
/// as a global "you are offline" banner (§ R11). `reason` is `undefined`
/// exactly when `available` is `true`; resolve it through `resolveLocalized`.
/// The wasm twin of the native `offlineAffordance()` UniFFI free fn.
#[wasm_bindgen(js_name = offlineAffordance)]
pub fn offline_affordance(kind: &str, connection_state: &str) -> Result<JsValue, JsValue> {
    #[derive(serde::Serialize)]
    struct Affordance {
        available: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<fauna_core::localized::LocalizedText>,
    }
    let verdict = fauna_protocol::offline_class::affordance(kind, connection_state);
    crate::rpc::to_js(&Affordance {
        available: verdict.is_available(),
        reason: verdict.reason(),
    })
}

/// `fauna_protocol::offline_class::is_online` → is this transport word one the
/// offline gate treats as **online**? The `connectionIsOnline` twin of the
/// native `connection_is_online` UniFFI free fn, and the same rule
/// [`offline_affordance`] applies — split out for the caller that has a
/// connection state but no kind in hand.
///
/// Its one consumer today is web's e2e automation surface, which publishes
/// `{state, online}` as the cross-app **connection barrier** observable
/// (`fauna_e2e_agent::CONNECTION_KEY`): every app greys an `OnlineOnly`
/// affordance while the word is offline and `"connecting"` is one of the
/// offline words, so a test driving an online-only control on a freshly loaded
/// SPA races the WS handshake and loses under load.
///
/// ⚠ **It exists so that nothing outside Rust writes `state === "connected"`.**
/// The gate's polarity is deliberately asymmetric — *online unless the word is
/// a KNOWN offline word*, so an older app meeting a future state word keeps its
/// controls live — and an equality test inverts that ruling in the direction
/// that **hangs**: a barrier waiting for `"connected"` blocks to its full
/// ceiling on exactly the case the gate was built to tolerate. Shipping the
/// boolean already decided keeps `OFFLINE_STATE_WORDS` with one owner, the same
/// bargain web already strikes for the affordance itself.
#[wasm_bindgen(js_name = connectionIsOnline)]
pub fn connection_is_online(connection_state: &str) -> bool {
    fauna_protocol::offline_class::is_online(connection_state)
}

/// `fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT` → the exact acknowledge
/// phrase the Backups immediate-delete modal requires the user to type (the nest
/// compares it byte-for-byte — no trim, no case-fold). NOT an i18n string: it is
/// locale-independent and pinned to the protocol constant so the modal can't
/// drift from the nest's check. The wasm twin of the native `immediate_delete_ack_text()`
/// FFI free fn (windows reads `FaunaFfiMethods.ImmediateDeleteAckText()`). See
/// `docs/goal/ui/backups.md` § User actions.
#[wasm_bindgen(js_name = immediateDeleteAckText)]
pub fn immediate_delete_ack_text() -> String {
    fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT.to_string()
}

/// `fauna_client_snapshots::immediate_delete_button_enabled` — the Backups
/// immediate-delete friction-bar predicate (`docs/goal/ui/backups.md`
/// Architectural rule 4): the retyped snapshot id AND the acknowledge phrase
/// must both match exactly, with no delete already in flight. `target_id`
/// empty means no snapshot selected (always disabled). The wasm twin of the
/// native FFI export; web's Backups page calls this instead of re-deriving
/// the four-way `&&` locally.
#[wasm_bindgen(js_name = immediateDeleteButtonEnabled)]
pub fn immediate_delete_button_enabled(
    deleting: bool,
    confirm_id: &str,
    target_id: &str,
    acknowledge_typed: &str,
) -> bool {
    fauna_client_snapshots::immediate_delete_button_enabled(
        deleting,
        confirm_id,
        target_id,
        acknowledge_typed,
    )
}

/// `fauna_client_snapshots::snapshot_restore_option_label` — a
/// `restore-snapshot-select` option's label: `"{kind} (#{id})"`, kind first
/// so two same-day snapshots are told apart by what they hold. `message_kind`
/// absent (a folder snapshot) reads as an empty kind. The wasm twin of the
/// native FFI export (linux/tui call the shared fn directly); web's Backups
/// page calls this instead of hand-rolling the same one-liner. `id` takes
/// `f64` (web's `number`) — mirrors every other snapshot-id wasm parameter.
#[wasm_bindgen(js_name = snapshotRestoreOptionLabel)]
pub fn snapshot_restore_option_label(message_kind: Option<String>, id: f64) -> String {
    fauna_client_snapshots::snapshot_restore_option_label(message_kind.as_deref(), id as i64)
}

/// `fauna_client_search::render::content_type_badge` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `L()` — the canonical, prefix-aware
/// + nest-accurate search-result badge map (so web shares the native apps'
/// map instead of its own `contentTypeLabel`). See `docs/goal/ui/search.md`.
#[wasm_bindgen(js_name = searchContentTypeBadge)]
pub fn search_content_type_badge(content_type: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_search::render::content_type_badge(
        content_type,
    ))
}

/// `fauna_core::content_category::content_label_style` → `{ label: {key, args},
/// icon, tint, accent }` — the shared content-label badge presentation map (the
/// canonical 5-category vocabulary + icon + two-tone hex colour), so the SPA's
/// `ContentLabelBadge` resolves `label` through `L()` and styles from `tint`/
/// `accent` instead of its own `category-*` CSS + `categoryText` maps (drift
/// #157). See `docs/goal/behavior/moderation.md` § Where logic lives.
#[wasm_bindgen(js_name = contentLabelStyle)]
pub fn content_label_style(category: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::content_category::content_label_style(category))
}

/// `fauna_core::content_category::primary_content_label` → the one entry of a
/// post's or message's `labels` (`[{ category, confidence_per_mille }]`) that wins
/// the visible `content-label-badge` — the highest-confidence one, a tie going to
/// the last — or `undefined` when there are none. One shared pick for the feed
/// card and the DM bubble (moderation.md § Per-row badge data path); the browser
/// twin of the UniFFI `primary_content_label` face.
#[wasm_bindgen(js_name = primaryContentLabel)]
pub fn primary_content_label(labels: JsValue) -> Result<JsValue, JsValue> {
    let labels: Vec<fauna_core::content_category::ContentLabelEntry> =
        serde_wasm_bindgen::from_value(labels).map_err(crate::rpc::err_to_js)?;
    match fauna_core::content_category::primary_content_label(&labels) {
        Some(entry) => crate::rpc::to_js(entry),
        None => Ok(JsValue::UNDEFINED),
    }
}

/// `fauna_core::ical::attendee_display` → `{ display_name: string, monogram:
/// string, secondary_email?: string }` — the shared `AttendeeRow` text projection
/// (CN→email fallback, monogram initial, email-beneath visibility), so the SPA
/// stops hand-rolling `attendeeMonogram` / `att.name || att.email`. See
/// events.md § Attendee list presentation.
#[wasm_bindgen(js_name = attendeeDisplay)]
pub fn attendee_display(name: &str, email: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::ical::attendee_display(name, email))
}

/// `fauna_core::ical::rsvp_status_label` → a `LocalizedText` `{ key, args }` the
/// SPA resolves through `resolveLocalized` — the canonical attendee RSVP
/// status→label map (`events.rsvp.*`, unknown → capitalized verbatim), so the SPA
/// stops hand-rolling its own `capitalizeStatus`. `status` is the verbatim
/// projected status string the attendee roster already carries. The trailing
/// color stays an idiomatic per-app render. See events.md § Attendee list
/// presentation.
#[wasm_bindgen(js_name = rsvpStatusLabel)]
pub fn rsvp_status_label(status: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::ical::rsvp_status_label(status))
}

/// `fauna_core::ical::reminder_label` → a `LocalizedText` `{ key, args }` the SPA
/// resolves through `resolveLocalized` — the canonical reminder preset→label map
/// (`PT15M`/`PT1H`/`P1D` → `events.reminder.{min_15,hour_1,day_1}`; a non-preset
/// offset falls back to the raw value rendered verbatim) the native apps
/// consume, so web stops hand-rolling its own `REMINDER_PRESETS` label strings +
/// `reminderLabel`. See `docs/goal/ui/events.md` § Reminders.
#[wasm_bindgen(js_name = reminderLabel)]
pub fn reminder_label(offset: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::ical::reminder_label(offset))
}

/// `fauna_core::ical::reminder_presets` → the canonical `PT15M`/`PT1H`/`P1D`
/// reminder preset catalog in picker order, as a JS array of
/// `{ value, label }` where `value` is the ISO-8601 offset the `<select>`
/// writes (the cross-app `select(id, "PT1H")` e2e contract — never
/// localized) and `label` is the `reminderLabel` `LocalizedText` the SPA
/// resolves through `resolveLocalized`. The ONE list every app's reminder
/// picker renders, so the SPA's local `REMINDER_PRESETS` value array retires.
/// See `docs/goal/ui/events.md` § Reminders.
#[wasm_bindgen(js_name = reminderPresets)]
pub fn reminder_presets() -> Result<JsValue, JsValue> {
    #[derive(serde::Serialize)]
    struct ReminderPreset {
        value: String,
        label: fauna_core::localized::LocalizedText,
    }
    let presets: Vec<ReminderPreset> = fauna_core::ical::reminder_presets()
        .into_iter()
        .map(|p| ReminderPreset {
            value: p.value,
            label: p.label,
        })
        .collect();
    crate::rpc::to_js(&presets)
}

/// `fauna_core::format::contact_status_label` → a `LocalizedText` `{ key, args }`
/// the SPA resolves through `resolveLocalized` — the canonical contact
/// relationship status→label map (`pending`/`accepted`/`confirmed`/`blocked` →
/// `common.*`, unknown → capitalized verbatim), so the SPA stops rendering the
/// raw lowercase `contact.status` string. `status` is the verbatim status the
/// contact row already carries. The status icon/color stays an idiomatic
/// per-app render. See contacts.md § Where logic lives → Status badge text.
#[wasm_bindgen(js_name = contactStatusLabel)]
pub fn contact_status_label(status: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::contact_status_label(status))
}

/// `fauna_core::format::bunker_app_label` → a `LocalizedText` `{ key, args }`
/// the SPA resolves through `resolveLocalized` — a `nostr-bunker-app-item`
/// row's primary label (the app's own `label` verbatim, else a
/// status-derived placeholder). See nostr.md § The nest as the user's NIP-46
/// signer.
#[wasm_bindgen(js_name = bunkerAppLabel)]
pub fn bunker_app_label(label: &str, status: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::bunker_app_label(label, status))
}

/// `fauna_core::format::bunker_last_used_label` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — a
/// `nostr-bunker-app-item` row's last-used sub-label. `formatted_time` is the
/// caller's own already-formatted time string (`undefined`/empty when never
/// used); JS optional params come through as `Option<String>` here.
#[wasm_bindgen(js_name = bunkerLastUsedLabel)]
pub fn bunker_last_used_label(formatted_time: Option<String>) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::bunker_last_used_label(
        formatted_time.as_deref(),
    ))
}

/// `fauna_core::format::claim_status_label` → a `LocalizedText` `{ key, args }`
/// the SPA resolves through `resolveLocalized` — the §5 manual-claims list
/// status badge (redeemed / voided / unredeemed).
/// Shared rather than hand-rolled because the two wire booleans are
/// independent and **redeemed wins over voided**; a client branching the other
/// way would silently disagree with its siblings. See monetization.md
/// § Pillar 3.
// `payments`-gated doc line — see § Gated element ids live in gated DOC LINES.
#[cfg_attr(
    feature = "payments",
    doc = " The badge is `subscription-claim-status[i]`."
)]
#[wasm_bindgen(js_name = claimStatusLabel)]
pub fn claim_status_label(redeemed: bool, voided: bool) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::claim_status_label(redeemed, voided))
}

/// `fauna_core::format::provider_status_label` → a `LocalizedText` `{ key, args }`
/// the SPA resolves through `resolveLocalized` — the §4 provider row status
/// badge (`configured` / `verified` / `error`; monetization.md § Pillar 3 →
/// "Provider status — evidence-based, no ping", ratified 2026-07-16).
/// Evidence-based, never an active probe — derives purely from
/// `ProviderItem.{last_verified_at,last_rejected_at}` (epoch seconds; `None` =
/// no evidence yet). Shared so web doesn't re-derive the most-recent-wins
/// branch the five native apps also consume via UniFFI.
#[wasm_bindgen(js_name = providerStatusLabel)]
pub fn provider_status_label(
    last_verified_at: Option<u64>,
    last_rejected_at: Option<u64>,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::provider_status_label(
        last_verified_at,
        last_rejected_at,
    ))
}

/// `fauna_core::format::unknown_sender_options` → the canonical
/// `unknown_sender_mail` picker options (`{ value, label }`, `label` a
/// `LocalizedText`), in the ratified order — the web consume leg of the
/// reach-policy formatting lift (family-safety.md § Where logic lives).
#[wasm_bindgen(js_name = unknownSenderOptions)]
pub fn unknown_sender_options() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::unknown_sender_options())
}

/// `fauna_core::format::feed_sources_options` → the canonical `feed_sources`
/// picker options, in the ratified order. See [`unknown_sender_options`].
#[wasm_bindgen(js_name = feedSourcesOptions)]
pub fn feed_sources_options() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::feed_sources_options())
}

/// `fauna_core::format::unknown_sender_label` → the localized label for a
/// stored `unknown_sender_mail` wire value, failing closed to `hold` for
/// anything unrecognized — never the permissive `allow`.
#[wasm_bindgen(js_name = unknownSenderLabel)]
pub fn unknown_sender_label(value: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::unknown_sender_label(value))
}

/// `fauna_core::format::feed_sources_label` → the localized label for a
/// stored `feed_sources` wire value, failing closed to `block`.
#[wasm_bindgen(js_name = feedSourcesLabel)]
pub fn feed_sources_label(value: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::feed_sources_label(value))
}

/// `fauna_core::format::unknown_peer_dm_options` → the canonical
/// `unknown_peer_dm` picker options, in the ratified order. See
/// [`unknown_sender_options`].
#[wasm_bindgen(js_name = unknownPeerDmOptions)]
pub fn unknown_peer_dm_options() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::unknown_peer_dm_options())
}

/// `fauna_core::format::unknown_peer_dm_label` → the localized label for a
/// stored `unknown_peer_dm` wire value, failing closed to `hold`.
#[wasm_bindgen(js_name = unknownPeerDmLabel)]
pub fn unknown_peer_dm_label(value: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::unknown_peer_dm_label(value))
}

/// `fauna_core::format::reach_policy_summary` → the five-line read-only
/// reach-policy summary the supervised side sees (`family-policy-summary`),
/// in the ratified display order. All three string knobs fail closed via
/// [`unknown_sender_label`] / [`feed_sources_label`] / [`unknown_peer_dm_label`].
///
/// `unknown_peer_dm` is `Option<String>` (JS `string | null | undefined`) —
/// absent means the knob is at its `allow` default, which is not the
/// fail-closed case (see the core fn's docs). All params are string/bool, so
/// none of them cross the wasm `i64`→JS `bigint` trap.
#[wasm_bindgen(js_name = reachPolicySummary)]
pub fn reach_policy_summary(
    contact_approval: bool,
    unknown_sender_mail: &str,
    federation_contact: bool,
    feed_sources: &str,
    unknown_peer_dm: Option<String>,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::reach_policy_summary(
        contact_approval,
        unknown_sender_mail,
        federation_contact,
        feed_sources,
        unknown_peer_dm.as_deref(),
    ))
}

/// `fauna_core::format::content_floor_options` → the canonical guardian
/// content-floor picker options (`inherit | collapse | block`, `{ value, label }`
/// with `label` a `LocalizedText`), in the ratified order — the web consume leg
/// of family-safety.md § Content policy. Mirrors [`unknown_sender_options`].
#[wasm_bindgen(js_name = contentFloorOptions)]
pub fn content_floor_options() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::content_floor_options())
}

/// `fauna_core::format::content_floor_label` → the localized label for a stored
/// guardian content-floor wire value, failing closed to `block` for anything
/// unrecognized — the same fail-closed rule as the reach knobs.
#[wasm_bindgen(js_name = contentFloorLabel)]
pub fn content_floor_label(value: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::content_floor_label(value))
}

/// `fauna_client_bridges::nostr_content_toggle_options` → the five Nostr
/// content-publishing toggles as one table (`{ key, ui_id, default_on, label,
/// subtitle }`, `label`/`subtitle` `LocalizedText`), in the ratified render
/// order — the web consume leg of `docs/goal/ui/nostr.md` § Where logic lives.
///
/// Before this the same five rows were spelled three times inside
/// `NostrSettingsSection.svelte` alone (read, write, and label render), and
/// once more in each of the other six apps. Mirrors `roleAddressOptions` —
/// hand the whole vocabulary across, the app owns only the widget.
#[wasm_bindgen(js_name = nostrContentToggleOptions)]
pub fn nostr_content_toggle_options() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_bridges::nostr_content_toggle_options())
}

/// `fauna_atproto_settings_machine::depth_level_options` → the four Bluesky
/// integration-depth rungs (`{ level, ui_id, title, description, hosted }`),
/// in ladder order — the web consume leg of `docs/goal/ui/atproto.md` § Where
/// logic lives, which already declared the machine the owner of "level logic …
/// all of it" while every app still carried its own copy of the table.
///
/// `hosted` replaces the per-app `level.startsWith('hosted')` sniff: whether a
/// rung is subject to the hosted gate is the catalog's fact, not a naming
/// coincidence each surface re-reads.
#[wasm_bindgen(js_name = atprotoDepthLevelOptions)]
pub fn atproto_depth_level_options() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_atproto_settings_machine::depth_level_options())
}

/// The client render verdict for a piece of content — the web twin of linux
/// `content_policy::verdict_for` (family-safety.md § Content policy). Composes,
/// **strictest-wins**, the viewer's OWN spam/phishing thresholds (→ `Collapse`,
/// the every-user un-darking of moderation.md § item 1) with, when supervised,
/// the guardian's per-category floor (`collapse`/`block`), and returns one of
/// `"show" | "badge" | "collapse" | "block"`.
///
/// Rule assembly stays entirely in shared Rust (priority #2) — this is a thin
/// marshalling shell over `fauna_core::obligation::render_verdict_composed`,
/// which every app's render path routes through. The caller passes
/// only the ingredients it already holds: `labels` (the reduced
/// `[{ category, confidence_per_mille }]` shape on `PostSummary.labels` /
/// `MessageSnapshot.labels`), the guardian `content_policy` object straight off
/// `familyStatus()` (or `null`/`undefined` for an unsupervised viewer — deserialized
/// to `Option<ContentPolicy>`, so `ContentFloor`'s `#[serde(other)]` keeps an
/// unparseable floor fail-closed to `block`), and the viewer's own per-mille
/// thresholds (both `undefined` until `spamGetPreferences` lands → no own-threshold
/// rule composes). `u16` params, so no `i64`→`bigint` trap.
#[wasm_bindgen(js_name = contentRenderVerdict)]
pub fn content_render_verdict(
    labels: JsValue,
    content_policy: JsValue,
    own_spam_permille: Option<u16>,
    own_phishing_permille: Option<u16>,
) -> Result<String, JsValue> {
    let (labels, policy, own, regions) = content_verdict_inputs(
        labels,
        content_policy,
        own_spam_permille,
        own_phishing_permille,
        JsValue::UNDEFINED,
    )?;
    Ok(
        fauna_core::obligation::render_verdict_composed(&labels, policy.as_ref(), own, &regions)
            .verdict
            .as_str()
            .to_string(),
    )
}

/// The full render decision — the verdict **and** the source that drove it —
/// composing all THREE strictest-wins sources, the region content policy
/// included (`region-blocking.md` § Where it composes — the render seam).
/// Returns `{ verdict, region }`, where `region` is
/// `{ region, authorityName, reasonCode, reason }` or `null`.
///
/// The web twin of the native `content_render_composed`, and a separate export
/// from [`content_render_verdict`] for the same reason: a blocked item needs the
/// attribution to name the authority, an ordinary render needs only the verb,
/// and every SPA render site already compares that verb as a bare string. A
/// surface moves to this one when it gains its region render — the same swap on
/// every app, so the two faces never become a per-app divergence.
///
/// `region_policies` is every region on the device's declared ancestor chain,
/// **most specific first**, straight off the region client;
/// `null`/`undefined`/omitted → no region rule composes, which is every device
/// today.
#[wasm_bindgen(js_name = contentRenderComposed)]
pub fn content_render_composed(
    labels: JsValue,
    content_policy: JsValue,
    own_spam_permille: Option<u16>,
    own_phishing_permille: Option<u16>,
    region_policies: JsValue,
) -> Result<JsValue, JsValue> {
    let (labels, policy, own, regions) = content_verdict_inputs(
        labels,
        content_policy,
        own_spam_permille,
        own_phishing_permille,
        region_policies,
    )?;
    composed_to_js(&fauna_core::obligation::render_verdict_composed(
        &labels,
        policy.as_ref(),
        own,
        &regions,
    ))
}

/// `contentRenderComposed` for one identified item, honouring the viewer's own
/// reports (`moderation.md` § Corollary — block also hides) — the wasm twin of
/// the UniFFI `content_render_for_item`. `hiddenContent` is the viewer's
/// `loadHiddenContent`; an item the viewer reported, or whose author they
/// reported, comes back `block` with `reported: true`.
#[wasm_bindgen(js_name = contentRenderForItem)]
#[allow(clippy::too_many_arguments)]
pub fn content_render_for_item(
    hidden_content: Vec<String>,
    item_id: String,
    author_id: Option<String>,
    labels: JsValue,
    content_policy: JsValue,
    own_spam_permille: Option<u16>,
    own_phishing_permille: Option<u16>,
    region_policies: JsValue,
) -> Result<JsValue, JsValue> {
    let (labels, policy, own, regions) = content_verdict_inputs(
        labels,
        content_policy,
        own_spam_permille,
        own_phishing_permille,
        region_policies,
    )?;
    composed_to_js(&fauna_core::obligation::render_verdict_for_item(
        &hidden_content,
        &item_id,
        author_id.as_deref(),
        &labels,
        policy.as_ref(),
        own,
        &regions,
    ))
}

fn composed_to_js(composed: &fauna_core::obligation::ComposedVerdict) -> Result<JsValue, JsValue> {
    let region = composed.region().map(|a| {
        serde_json::json!({
            "region": a.region.as_str(),
            "authorityName": a.authority_name,
            "reasonCode": a.reason_code,
            "reason": a.reason,
        })
    });
    crate::rpc::to_js(&serde_json::json!({
        "verdict": composed.verdict.as_str(),
        "region": region,
        "reported": composed.reported(),
    }))
}

/// The deserialization both content-verdict exports do, so their tolerance for
/// absent inputs cannot drift apart.
#[allow(clippy::type_complexity)]
fn content_verdict_inputs(
    labels: JsValue,
    content_policy: JsValue,
    own_spam_permille: Option<u16>,
    own_phishing_permille: Option<u16>,
    region_policies: JsValue,
) -> Result<
    (
        Vec<fauna_core::content_category::ContentLabelEntry>,
        Option<fauna_core::obligation::ContentPolicy>,
        Option<fauna_core::obligation::ViewerThresholds>,
        Vec<fauna_core::region_policy::RegionRuleSet>,
    ),
    JsValue,
> {
    use fauna_core::obligation::{ContentPolicy, ViewerThresholds};
    let labels: Vec<fauna_core::content_category::ContentLabelEntry> = crate::rpc::from_js(labels)?;
    let policy: Option<ContentPolicy> = if content_policy.is_null() || content_policy.is_undefined()
    {
        None
    } else {
        Some(crate::rpc::from_js(content_policy)?)
    };
    let own = own_spam_permille
        .zip(own_phishing_permille)
        .map(|(s, p)| ViewerThresholds {
            spam_permille: s,
            phishing_permille: p,
        });
    let regions: Vec<fauna_core::region_policy::RegionRuleSet> =
        if region_policies.is_null() || region_policies.is_undefined() {
            Vec::new()
        } else {
            crate::rpc::from_js(region_policies)?
        };
    Ok((labels, policy, own, regions))
}

/// The guardian-floor categories a piece of content triggers, for **Guardian
/// Notify** counting (family-safety.md § Guardian Notify) — the web twin of the
/// shared `fauna_core::obligation::guardian_enforced_categories`. Returns the
/// subset of `["nsfw","spam","phishing","commercial"]` whose guardian floor bites
/// on this item (the categories for which "the guardian's policy is acting"). The
/// ward's own-threshold collapses are deliberately excluded — Notify is a lens on
/// the *guardian's* policy, not the ward's own choices — so this takes only the
/// guardian `content_policy`, never the viewer's thresholds.
///
/// Same reduced inputs as [`content_render_verdict`]: `labels` (the
/// `[{ category, confidence_per_mille }]` shape on `PostSummary.labels` /
/// `MessageSnapshot.labels`) and the guardian `content_policy` object straight off
/// `familyStatus()` (or `null`/`undefined` for an unsupervised viewer → empty
/// result). Returns a `string[]` of category names.
#[wasm_bindgen(js_name = guardianEnforcedCategories)]
pub fn guardian_enforced_categories_js(
    labels: JsValue,
    content_policy: JsValue,
) -> Result<JsValue, JsValue> {
    use fauna_core::obligation::ContentPolicy;
    let labels: Vec<fauna_core::content_category::ContentLabelEntry> = crate::rpc::from_js(labels)?;
    let cats: Vec<&'static str> = if content_policy.is_null() || content_policy.is_undefined() {
        Vec::new()
    } else {
        let policy: ContentPolicy = crate::rpc::from_js(content_policy)?;
        fauna_core::obligation::guardian_enforced_categories(&labels, &policy)
    };
    crate::rpc::to_js(&cats)
}

/// `fauna_core::obligation::NOTIFY_REPORT_MIN_INTERVAL_SECS` → the ≤hourly
/// **Guardian Notify** batch interval, so web batches identically to native
/// (priority #2 — one shared constant, no per-app drift). Returned as `u32`
/// seconds (no `i64`→`bigint` wasm trap).
#[wasm_bindgen(js_name = notifyReportMinIntervalSecs)]
pub fn notify_report_min_interval_secs() -> u32 {
    fauna_core::obligation::NOTIFY_REPORT_MIN_INTERVAL_SECS as u32
}

/// `fauna_client_drafts::autosave_debounce` → the shared draft-autosave
/// debounce window, so web coalesces composer edits on the same cadence as
/// the Rust-native shells (priority #2 — one shared window, no per-app
/// drift; `docs/goal/behavior/reserved-folders.md` § Drafts Sync). Returned
/// as `u32` milliseconds (no `i64`→`bigint` wasm trap).
///
/// It reads the shared accessor rather than the constant behind it, for one
/// door with the native shells — but **the harness's window seam cannot reach
/// web through it**: `wasm32-unknown-unknown` has no process environment, so
/// `std::env::var` there is always `Err(NotPresent)` and this face answers the
/// constant on every web build, test-capable or not. Web's own 150 ms
/// `__faunaTestAgent` override in `$lib/{conversations,feed,event-drafts}` is
/// today's substitute, and it SHORTENS where the seam lengthens, so web's
/// leave-flush leg (`test_web_leave_flush.py`) still races the debounce rather
/// than ruling it out.
#[wasm_bindgen(js_name = autosaveDebounceMs)]
pub fn autosave_debounce_ms() -> u32 {
    fauna_client_drafts::autosave_debounce().as_millis() as u32
}

/// `ReachPolicy::summary_lines()` → the full read-only ward policy summary
/// (`family-policy-summary`): the four reach-knob lines PLUS one line per
/// non-inherit guardian content floor and, when on, the Notify line
/// (family-safety.md § Content policy / Guardian Notify — *"the ward's read-only
/// summary renders the content rules exactly as it renders the reach knobs"*).
/// The web twin of the linux ward summary — pass the whole wire `ReachPolicy`
/// object straight off `familyStatus()`. A superset of [`reach_policy_summary`],
/// which older web code called with only the four v1 knobs (so content floors
/// never showed).
#[wasm_bindgen(js_name = reachPolicySummaryFull)]
pub fn reach_policy_summary_full(policy: JsValue) -> Result<JsValue, JsValue> {
    let policy: fauna_protocol::family::ReachPolicy = crate::rpc::from_js(policy)?;
    crate::rpc::to_js(&policy.summary_lines())
}

/// `fauna_core::format::content_notice_line` → the guardian's per-ward **Guardian
/// Notify** readout line (`family-ward-content-notices`) for one `(category,
/// count)` notice off a ward's `content_notices`: a `{ label, value }`
/// [`PolicySummaryLine`](fauna_core::format::PolicySummaryLine) the caller resolves
/// like any other — `label` the localized category name, `value` the "N flagged
/// today" count. Category + count only; the web twin of the native
/// `content_notice_line` (linux calls it directly). `u32` count (no `bigint` trap).
#[wasm_bindgen(js_name = contentNoticeLine)]
pub fn content_notice_line(category: &str, count: u32) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::content_notice_line(category, count))
}

/// `fauna_core::format::usage_today_line` → the screen-time usage readout
/// (`family-ward-usage-today`) for one ward's day, as a `{ label, value }`
/// [`PolicySummaryLine`](fauna_core::format::PolicySummaryLine) resolved like
/// any other. `budget_minutes` absent renders the bare figure.
///
/// The SAME call backs the guardian's per-ward row and the ward's own summary,
/// which is what makes the goal doc's transparency promise ("the ward's summary
/// shows the same number") structural rather than a convention two surfaces
/// could drift from. `u32`/`u16` (no `i64`→`bigint` wasm trap).
#[wasm_bindgen(js_name = usageTodayLine)]
pub fn usage_today_line(
    used_minutes: u32,
    budget_minutes: Option<u16>,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::usage_today_line(
        used_minutes,
        budget_minutes,
    ))
}

/// `fauna_core::screen_time::UsageHeartbeat` → the ward client's screen-time
/// heartbeat (`family-safety.md` § Screen time, Slice E), as a stateful handle
/// the SPA holds for the session.
///
/// Every rule that decides a child's screen time — what counts as use, when to
/// flush, what a failed report owes, how the nest's cross-device total combines
/// with minutes this device has not sent yet — lives in shared Rust behind this
/// wrapper, exactly as the lock decision does. Web contributes only what a
/// browser alone knows: the clock, the tab's visibility, and the device's UTC
/// offset.
///
/// **All epoch values cross as `f64`, never `i64`** — a wasm `i64` parameter
/// arrives as a JS `bigint` and throws a `TypeError` on an ordinary number
/// (`Date.now() / 1000`), a trap this codebase has paid for before.
#[wasm_bindgen(js_name = UsageHeartbeat)]
#[derive(Default)]
pub struct WasmUsageHeartbeat {
    inner: fauna_core::screen_time::UsageHeartbeat,
}

#[wasm_bindgen(js_class = UsageHeartbeat)]
impl WasmUsageHeartbeat {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adopt the ward's screen-time policy from `familyStatus()`. Pass the wire
    /// `screen_time` object; `null`/absent means no budget, which drops all
    /// accounting state.
    #[wasm_bindgen(js_name = setPolicy)]
    pub fn set_policy(&mut self, policy: JsValue) -> Result<(), JsValue> {
        let policy: Option<fauna_core::screen_time::ScreenTimePolicy> =
            crate::rpc::from_js(policy)?;
        self.inner.set_policy(policy.as_ref());
        Ok(())
    }

    /// Whether a daily budget is set — i.e. whether the heartbeat should run at
    /// all. No budget, no accounting (goal doc § Screen time).
    #[wasm_bindgen(js_name = isAccounting)]
    pub fn is_accounting(&self) -> bool {
        self.inner.is_accounting()
    }

    /// Record whether the app is being used: `active` = the tab is visible AND
    /// the screen-time lock is not showing. Lock-screen time is not use.
    #[wasm_bindgen(js_name = setActive)]
    pub fn set_active(&mut self, active: bool, now_secs: f64) {
        self.inner.set_active(active, now_secs as i64);
    }

    /// Seed the cross-device total from `familyStatus().usage_today_minutes`,
    /// so the very first paint can evaluate the budget.
    #[wasm_bindgen(js_name = seedTotal)]
    pub fn seed_total(&mut self, usage_today_minutes: Option<u32>) {
        self.inner.seed_total(usage_today_minutes);
    }

    /// The minutes to send with `familyUsageReport` now, or `undefined` for
    /// "nothing due". `0` is a real answer — a zero-minute report is a read.
    /// The caller MUST answer with `reportSucceeded`/`reportFailed`.
    #[wasm_bindgen(js_name = takeDue)]
    pub fn take_due(&mut self, now_secs: f64) -> Option<u32> {
        self.inner.take_due(now_secs as i64)
    }

    /// Land a `familyUsageReport` reply.
    #[wasm_bindgen(js_name = reportSucceeded)]
    pub fn report_succeeded(&mut self, day: f64, day_total_minutes: u32, now_secs: f64) {
        self.inner
            .report_succeeded(day as i64, day_total_minutes, now_secs as i64);
    }

    /// A report that never landed — its minutes go back on the pile.
    #[wasm_bindgen(js_name = reportFailed)]
    pub fn report_failed(&mut self) {
        self.inner.report_failed();
    }

    /// The figure to hand `screenLockMessage` and to display: the nest total
    /// plus what this device has accrued since. `undefined` until a total has
    /// been heard (the ratified fail-open on the budget arm).
    #[wasm_bindgen(js_name = usedTodayMinutes)]
    pub fn used_today_minutes(&self, now_secs: f64) -> Option<u32> {
        self.inner.used_today_minutes(now_secs as i64)
    }

    /// Drop everything on an identity change — sign-out, account switch, reset.
    #[wasm_bindgen(js_name = reset)]
    pub fn reset(&mut self) {
        self.inner.reset();
    }
}

/// `fauna_core::screen_time::screen_lock_message` → the ward's full-screen
/// `screen-time-lock` decision **and** its `screen-time-lock-message`, in one
/// call (family-safety.md § Screen time, Slice E): `null` = render no lock,
/// otherwise a `LocalizedText` `{ key, args }` the SPA resolves through
/// `resolveLocalized`. Pass the whole wire `screen_time` object off
/// `familyStatus().policy`; `null`/absent is the unsupervised-equivalent
/// default and never locks.
///
/// The gating decision deliberately does NOT cross into JS — web asks this one
/// question exactly as linux asks `screen_lock_message` directly, so the two
/// reference legs cannot drift on when a child is locked out (priority #2).
/// `now_local_minutes` is minutes since the device's local midnight;
/// `used_today_minutes` is the day's cross-device total from the last
/// `familyUsageReport` reply, or `null` when not yet known. Both `u16`/`u32`
/// (no `i64`→`bigint` wasm trap).
#[wasm_bindgen(js_name = screenLockMessage)]
pub fn screen_lock_message(
    policy: JsValue,
    now_local_minutes: u16,
    used_today_minutes: Option<u32>,
    guardian_handle: &str,
) -> Result<JsValue, JsValue> {
    let policy: Option<fauna_core::screen_time::ScreenTimePolicy> = crate::rpc::from_js(policy)?;
    let policy = policy.unwrap_or_default();
    crate::rpc::to_js(&fauna_core::screen_time::screen_lock_message(
        &policy,
        now_local_minutes,
        used_today_minutes,
        guardian_handle,
    ))
}

/// `fauna_core::screen_time::parse_time_of_day` → a guardian-typed `"HH:MM"`
/// window bound as minutes from local midnight, for the two
/// `family-policy-screen-window-*-input`s. Returns `null` for an empty input
/// ("this bound is unset" — how a guardian clears the window) and **throws**
/// the reason string for anything unparseable, which the editor surfaces on
/// its `error-message`. Web parses through shared Rust rather than in JS so
/// all 7 apps agree on what `"9:5"` / `"24:00"` / `"08:60"` mean.
#[wasm_bindgen(js_name = parseTimeOfDay)]
pub fn parse_time_of_day(input: &str) -> Result<Option<u16>, JsValue> {
    fauna_core::screen_time::parse_time_of_day(input).map_err(JsValue::from_str)
}

/// `fauna_core::screen_time::format_time_of_day` → minutes from local midnight
/// rendered back as `"HH:MM"`, to fill a guardian's editor from the stored
/// policy. The inverse of [`parse_time_of_day`].
#[wasm_bindgen(js_name = formatTimeOfDay)]
pub fn format_time_of_day(minutes_from_midnight: u16) -> String {
    fauna_core::screen_time::format_time_of_day(minutes_from_midnight)
}

/// `fauna_core::screen_time::parse_daily_minutes` → a guardian-typed daily
/// budget in whole minutes for `family-policy-screen-daily-minutes-input`.
/// `null` for empty (no budget); **throws** the reason string outside
/// `0..=1440`, the same bound the nest enforces at `policy.update`.
#[wasm_bindgen(js_name = parseDailyMinutes)]
pub fn parse_daily_minutes(input: &str) -> Result<Option<u16>, JsValue> {
    fauna_core::screen_time::parse_daily_minutes(input).map_err(JsValue::from_str)
}

/// `fauna_core::format::approval_display_text` → what a `family-approval-item`
/// row should display verbatim, or `null` when the caller should render its
/// own localized no-sender placeholder (`family.approval_no_sender`).
#[wasm_bindgen(js_name = approvalDisplayText)]
pub fn approval_display_text(
    kind: &str,
    peer_address: &str,
    peer_handle: &str,
    summary: &str,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::approval_display_text(
        kind,
        peer_address,
        peer_handle,
        summary,
    ))
}

/// `fauna_core::format::thread_label_display` → a `LocalizedText` `{ key, args }`
/// the SPA resolves through `resolveLocalized` — a blank/whitespace-only thread
/// label carries the canonical `conversations.detail.no_subject` key
/// (`"(no subject)"`); a non-empty label rides verbatim. So web stops rendering an
/// empty `thread.label`/`detail.label` as blank (a latent bug) and shares android's
/// richest fallback. The raw label stays the filter/sort/rename value. See
/// conversations.md § Where logic lives → thread label display.
#[wasm_bindgen(js_name = threadLabelDisplay)]
pub fn thread_label_display(label: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::thread_label_display(label))
}

/// `fauna_core::format::media_sort_label` → a `LocalizedText` `{ key, args }` the
/// SPA resolves through `resolveLocalized` — the `media-sort-select` value
/// (`"name"`/`"size"`/`"date"`) → label decision, so web shares it instead of
/// its own `switch`. See `docs/goal/ui/media.md` § Layout & flow.
#[wasm_bindgen(js_name = mediaSortLabel)]
pub fn media_sort_label(value: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::media_sort_label(value))
}

/// `fauna_core::format::media_sort_direction_label` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — the
/// `media-sort-direction` `descending` flag → label decision.
#[wasm_bindgen(js_name = mediaSortDirectionLabel)]
pub fn media_sort_direction_label(descending: bool) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::media_sort_direction_label(descending))
}

/// `fauna_core::format::os_maintenance_status_label` → a `LocalizedText` `{ key, args }`
/// the SPA resolves through `resolveLocalized` — the `admin-nest`
/// `nest-os-maintenance-status` line (reboot-pending / updates-pending / up-to-date,
/// from the `os_*` fields on `fauna.setup.status`), so web shares the state→key
/// decision instead of its own ternary. See `installers/vps.md` § Host OS Maintenance.
#[wasm_bindgen(js_name = osMaintenanceStatusLabel)]
pub fn os_maintenance_status_label(
    security_updates_pending: u32,
    reboot_pending: bool,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::os_maintenance_status_label(
        security_updates_pending,
        reboot_pending,
    ))
}

/// `fauna_core::format::mail_health_state_label` → a `LocalizedText` `{ key, args }`
/// for the `admin-mail-health-status` line from the `fauna.bridges.mail_health`
/// reply's open-enum `state` (unknown → the generic "needs attention" key). See
/// `mail-deliverability.md` § The mail health readout.
#[wasm_bindgen(js_name = mailHealthStateLabel)]
pub fn mail_health_state_label(state: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::mail_health_state_label(state))
}

/// `fauna_core::format::mail_health_check_state_label` → a `LocalizedText`
/// `{ key, args }` for one `admin-mail-health-check-state` (`pass` / `warn` /
/// `fail` / `info`; unknown → "needs attention").
#[wasm_bindgen(js_name = mailHealthCheckStateLabel)]
pub fn mail_health_check_state_label(state: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::mail_health_check_state_label(state))
}

/// `fauna_core::format::contact_matches_filter` → does a contact roster row match
/// the `contacts-search-field` query? Case-insensitive substring of the trimmed
/// query over `handle` / `domain` / hex `actor_id` (empty query matches all), so
/// web filters the roster through the same predicate the native apps use instead
/// of its own actor-id-only match. `handle`/`domain` are `null`/`undefined` for a
/// federated peer. Local-only — never a nest query. See `docs/goal/ui/contacts.md`
/// § Where logic lives → Contact roster filter.
#[wasm_bindgen(js_name = contactMatchesFilter)]
pub fn contact_matches_filter(
    query: &str,
    handle: Option<String>,
    domain: Option<String>,
    actor_id: &str,
) -> bool {
    fauna_core::format::contact_matches_filter(
        query,
        handle.as_deref(),
        domain.as_deref(),
        actor_id,
        // The private overlay's nickname + labels join this face with web's
        // leg of contacts.md § The private overlay (ordered behind its
        // account-store backend).
        None,
        &[],
    )
}

/// `fauna_core::format::contact_toggle_block_label` → a `LocalizedText` `{ key, args }`
/// the SPA resolves through `resolveLocalized` — the `profile-block-button` toggle
/// label (`profile.unblock` when already blocked, else `profile.block`), so web shares
/// the toggle wording instead of its own `$derived` ternary. The button *style* stays
/// web-local. See `docs/goal/ui/profile.md` § Element table and contacts.md § Where
/// logic lives → Unblock.
#[wasm_bindgen(js_name = contactToggleBlockLabel)]
pub fn contact_toggle_block_label(is_blocked: bool) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::contact_toggle_block_label(is_blocked))
}

/// `fauna_core::format::follow_toggle_label` → a `LocalizedText` `{ key, args }` the SPA
/// resolves through `resolveLocalized` — the `profile-follow-button` label
/// (`profile.following` when the viewer already follows, else `profile.follow`), so web
/// shares the toggle wording instead of its own `isFollowing` ternary. The `isFollowing`
/// derivation (subscription status / optimistic flip) stays web-local by ratified
/// decision. See `docs/goal/ui/profile.md` § Element table → `profile-follow-button`.
#[wasm_bindgen(js_name = followToggleLabel)]
pub fn follow_toggle_label(is_following: bool) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::follow_toggle_label(is_following))
}

/// `fauna_conversations::compose::recipient_resolve_status_from_variant` → a
/// `ResolveStatusView` `{ token, label }` the SPA renders on the
/// `recipient-resolve-status` element: `token` (kebab-case state, drives
/// `data-state`) + `label` (a `LocalizedText` resolved through `resolveLocalized`;
/// `null` for idle → empty text). Takes the serde variant name (`"Resolved"`,
/// `"NotFound"`, …) exactly as the compose snapshot's `resolve_state` field
/// serializes — an unknown/absent value degrades to idle. Replaces web's twin
/// hand-rolled `resolveStateAttr`/`resolveStatusText` switches. See
/// `docs/goal/ui/conversations.md` § Errors & edge cases.
#[wasm_bindgen(js_name = recipientResolveStatus)]
pub fn recipient_resolve_status(state: Option<String>) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(
        &fauna_conversations::compose::recipient_resolve_status_from_variant(
            state.as_deref().unwrap_or(""),
        ),
    )
}

/// `fauna_conversations::snapshot::next_sort_order_from_variant` → the order one
/// tap of `conversation-sort` advances to, as the serde variant name `setSort`
/// already takes. Takes the name the snapshot's `sort` field serializes
/// (`"LatestActivity"` / `"OldestFirst"` / `"Unread"`), so the SPA's handler is
/// `setSort(nextSortOrder(snapshot.sort))` — it never enumerates the orders.
/// An unknown/absent value restarts the cycle at the default rather than
/// erroring (a tap always moves), which is why this does not mirror `setSort`'s
/// reject-on-unknown. Replaces web's hand-rolled 2-way ternary, which left the
/// `Unread` order unreachable. See `docs/goal/ui/conversations.md` § Where logic
/// lives + the `conversation-sort` element-table row.
#[wasm_bindgen(js_name = nextSortOrder)]
pub fn next_sort_order(current: Option<String>) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(
        &fauna_conversations::snapshot::next_sort_order_from_variant(
            current.as_deref().unwrap_or(""),
        ),
    )
}

/// `fauna_conversations::quickset_emojis` → the reaction quick-set as a JS array of
/// strings, in the one shared order (`docs/goal/ui/conversations.md` § Reactions &
/// message delete). The wasm twin of the UniFFI face the native apps read; web is the
/// only client that cannot reach the const as a crate dep or across UniFFI.
///
/// Ordering is the contract, not just the membership: `dm-reaction-option` is an
/// indexed ui.yaml element, so an e2e that taps index 0 is asserting 👍 on all 7 apps.
/// The fuller "more" picker widget stays web-local (the one sanctioned per-platform
/// divergence, § Rendering / picker glue); its shortcut-grid list is `moreGridEmojis`.
#[wasm_bindgen(js_name = quicksetEmojis)]
pub fn quickset_emojis() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_conversations::quickset_emojis())
}

/// `fauna_conversations::more_grid_emojis` → the twenty-emoji shortcut grid shown beside
/// the fuller picker's free-entry field, as a JS array of strings — the list web and
/// windows used to hand-copy (`docs/goal/ui/conversations.md` § Reactions & message
/// delete → *Rendering / picker glue*).
#[wasm_bindgen(js_name = moreGridEmojis)]
pub fn more_grid_emojis() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_conversations::more_grid_emojis())
}

/// `fauna_core::format::device_status_label` → a `LocalizedText` `{ key, args }` the SPA
/// resolves through `resolveLocalized` — the Devices page `device-status` label
/// (`devices.online` when the device's `online` flag is set, else `devices.offline`), so
/// web shares the online→label map instead of its own ternary. The status dot *color*
/// stays web-local. See `docs/goal/ui/devices.md` § Where logic lives and § Element table.
#[wasm_bindgen(js_name = deviceStatusLabel)]
pub fn device_status_label(online: bool) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::device_status_label(online))
}

/// `fauna_core::format::device_place_label` → a `LocalizedText` `{ key, args }` the
/// SPA resolves through `resolveLocalized` — the Devices page `device-folder-role-badge`
/// chip for one device place's three flags (`DeviceFolderRole`), composed from the
/// wizard's `devices.wizard.place_*` labels (the templates' arguments are keys, which
/// `resolveLocalized` translates). devices.md § Element table
/// (`device-folder-role-badge`).
#[wasm_bindgen(js_name = devicePlaceLabel)]
pub fn device_place_label(
    originates: bool,
    accepts: bool,
    applies_deletes: bool,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::device_place_label(
        originates,
        accepts,
        applies_deletes,
    ))
}

/// `fauna_core::format::mail_serving_status_label` → a `LocalizedText` `{ key, args }` the
/// SPA resolves through `resolveLocalized` — the admin Users page
/// `admin-users-mail-serving-status` label (`admin.users_page.serving_here` when the user's
/// `mail_serving_enabled` flag is set, else `serving_disabled`), so web shares the
/// enabled→label map instead of its own ternary (the same map windows/native consume). See
/// `docs/goal/behavior/admin.md` § Where logic lives.
#[wasm_bindgen(js_name = mailServingStatusLabel)]
pub fn mail_serving_status_label(enabled: bool) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::mail_serving_status_label(enabled))
}

/// `fauna_core::format::dns_verdict_label` → a `LocalizedText` `{ key, args }` the SPA
/// resolves through `resolveLocalized` — the `admin-dns` record-matrix verdict label
/// (`Ok`/`Missing`/`Mismatch` → `admin.dns.status_*`, else `status_checking`), so web
/// shares the verdict→key decision instead of its own `statusLabel` switch. Web passes
/// `r.verdict?.status ?? ''` and `r.verdict?.observed ?? []` (an absent verdict →
/// checking). `observed` is what public DNS actually served, which a `Mismatch`
/// renders into the label. The verdict CSS class (`ok`/`bad`/`checking`) stays
/// web-local. See value-formatting.md § DNS verdict label.
#[wasm_bindgen(js_name = dnsVerdictLabel)]
pub fn dns_verdict_label(status: &str, observed: Vec<String>) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::dns_verdict_label(status, &observed))
}

/// `fauna_core::format::cert_status_label` → a `LocalizedText` `{ key, args }` the SPA
/// resolves through `resolveLocalized` — the `admin-dns` served-cert health badge's
/// **state** word (`ValidTrusted` → `status_valid`, `Expiring` → `status_expiring`, else
/// `status_on_floor`), so web shares the state→key decision instead of its own
/// `certStatusText` switch. Only the state word — a badge wants [`cert_status_view`],
/// which composes this with the sub-label that follows it. See value-formatting.md
/// § Cert status badge.
#[wasm_bindgen(js_name = certStatusLabel)]
pub fn cert_status_label(state: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::cert_status_label(state))
}

/// `fauna_core::format::connection_state_label` → a `LocalizedText` `{ key, args }`
/// the SPA resolves through `resolveLocalized` — the global `connection-status`
/// indicator's label for a `connectionState()` word. Shares the state→key decision
/// with every other app instead of web re-deciding it in a ternary; in
/// particular it is what makes the fourth state (`unreachable` → "Cannot connect")
/// render as itself rather than falling into web's old "anything not
/// connected/connecting is Disconnected" default arm. See transport.md
/// § Connection-status indicator.
#[wasm_bindgen(js_name = connectionStateLabel)]
pub fn connection_state_label(state: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::connection_state_label(state))
}

/// `fauna_core::format::cert_status_view` → the whole `admin-dns` served-cert badge as
/// `{ state, show_self_signed, expires_at_unix }`: the state word plus which of the two
/// mutually-exclusive sub-labels follows it. The SPA renders
/// `` `${label} ${resolveLocalized(v.state)}` ``, then `(self_signed)` when
/// `show_self_signed`, or `expires({date})` when `expires_at_unix` is non-null —
/// formatting that epoch with `formatCertExpiry`. Never both: a floor cert's own
/// far-future expiry is withheld. The badge CSS class stays web-local.
///
/// `not_after_unix` crosses as `f64`, not `i64` — the boundary convention every
/// other epoch-taking export here follows ([`backup_last_upload_label`],
/// [`conversation_timestamp`]): wasm-bindgen maps an `i64` *parameter* to a JS
/// `bigint`, which the SPA's snapshot (serde-serialized, so a plain `number`)
/// cannot satisfy without a cast at every call site. See
/// value-formatting.md § Cert status badge.
#[wasm_bindgen(js_name = certStatusView)]
pub fn cert_status_view(
    state: &str,
    is_floor: bool,
    not_after_unix: f64,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::format::cert_status_view(
        state,
        is_floor,
        not_after_unix as i64,
    ))
}

/// `fauna_core::format::offer_status` + `offer_status_label` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — the
/// `subscription-offer-status` badge on another's profile
/// (`subscriptions.offer_status_*`, precedence Active>Pending>None). `status_tier`
/// is the viewer's confirmed held tier (`status.get`, `null` if none); `pending` a
/// transient post-click flag (`status.get` carries no pending discriminant). A
/// composite (web's Subscribe button is status-independent, so it needs only the
/// label — the natives consume the `OfferStatus` enum directly over UniFFI for the
/// button state too); web shares the precedence + label map instead of its own
/// inline `offerStatusLabel`. See `docs/goal/ui/profile.md` § Layout & flow /
/// `monetization.md` § Pillar 1.
#[wasm_bindgen(js_name = offerStatusLabel)]
pub fn offer_status_label(
    tier_name: &str,
    status_tier: Option<String>,
    pending: bool,
) -> Result<JsValue, JsValue> {
    let status = fauna_core::format::offer_status(tier_name, status_tier.as_deref(), pending);
    crate::rpc::to_js(&fauna_core::format::offer_status_label(status))
}

/// `fauna_core::format::contact_row_blocks_actor` → does this contact roster row mean
/// the `target_actor_id` is blocked (case-insensitive hex `peer_id` match AND
/// `status == "blocked"`)? Web folds it over `fauna.contacts.list` with `.some(..)` to
/// derive the `profile-block-button` state, sharing the predicate the native apps
/// use instead of its own `c.peer_id === id && c.status === 'blocked'` match.
/// Local-only. See `docs/goal/ui/contacts.md` § Where logic lives → Unblock.
#[wasm_bindgen(js_name = contactRowBlocksActor)]
pub fn contact_row_blocks_actor(
    row_peer_id: &str,
    row_status: &str,
    target_actor_id: &str,
) -> bool {
    fauna_core::format::contact_row_blocks_actor(row_peer_id, row_status, target_actor_id)
}

/// `fauna_client_mail_settings::bridge_display_name` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — the canonical
/// role→name map (MDA serves IMAP + CalDAV = "Mail & calendar bridge", MTA serves
/// SMTP only = "Mail bridge", unknown = "Bridge"), so web shares the native
/// apps' map instead of hand-rolling its own `bridgeDisplayName`. See
/// `docs/goal/architecture/apps/bridges.md` § Active bridges.
#[wasm_bindgen(js_name = bridgeDisplayName)]
pub fn bridge_display_name(role: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_mail_settings::bridge_display_name(role))
}

/// `fauna_client_bridges::nostr_key_source_label` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — the canonical
/// Nostr signing-mode→label map (`generated`/`imported`/`remote`/`nip07` →
/// `nostr.account.mode_*`, unknown/placeholder → verbatim), so the Nostr
/// settings page stops rendering the raw stored `mode` enum. `mode` is the
/// verbatim `fauna.bridges.list` `mode` field the snapshot already carries. See
/// `docs/goal/ui/nostr.md` § User actions → Signing-mode display.
#[wasm_bindgen(js_name = nostrKeySourceLabel)]
pub fn nostr_key_source_label(mode: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_bridges::nostr_key_source_label(mode))
}

/// `fauna_client_bridges::nostr_link_mode_label` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — the
/// request-mode twin of [`nostr_key_source_label`]: `generate`/`import`/
/// `remote`/`nip07`, the values the `nostr-link-mode` picker sends, → their
/// human label (unknown/placeholder → verbatim). `mode` is the picker's
/// current selection. See `docs/goal/ui/nostr.md` § Account linking →
/// *Link-mode picker labels*.
#[wasm_bindgen(js_name = nostrLinkModeLabel)]
pub fn nostr_link_mode_label(mode: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_bridges::nostr_link_mode_label(mode))
}

/// `fauna_client_bridges::is_unified_bridges_page_bridge` — whether a
/// `fauna.bridges.list` row belongs on the unified Bridges page (Nostr has
/// its own dedicated page; `bridges.md` § Scope). Pure, so a plain `bool`
/// return, no `to_js` wrapping needed.
#[wasm_bindgen(js_name = isUnifiedBridgesPageBridge)]
pub fn is_unified_bridges_page_bridge(id: &str) -> bool {
    fauna_client_bridges::is_unified_bridges_page_bridge(id)
}

/// `fauna_client_bridges::mode_applies` — does one declared link mode apply on
/// `platform`? A mode with no `platform` is universal; a scoped one applies
/// only on the app whose **canonical name** it names (the ui.yaml vocabulary —
/// web passes `"web"`). The shared module's docs own the rule.
///
/// Predicate-shaped like the UniFFI twin: web's bridge list lives in TS
/// (`$lib/bridges.ts` decodes it, no wasm round-trip), so the SPA's filter
/// keeps its own objects and swaps only the condition — the piece that was
/// hand-written seven times. Web's nip07 extension probe stays local, ANDed
/// after this: a runtime capability no shared crate can observe.
#[wasm_bindgen(js_name = bridgeModeApplies)]
pub fn bridge_mode_applies(mode_platform: Option<String>, platform: String) -> bool {
    fauna_client_bridges::mode_applies(mode_platform.as_deref(), &platform)
}

/// `fauna_client_bridges::link_block` — why a bridge's Link control is not
/// actionable (`bridges.md` § Errors & edge cases). Returns `null` when it **is**
/// actionable, else `{ kind: "provider_error", message }` (the nest's own
/// explanation — render verbatim) or `{ kind: "no_applicable_mode" }` (render the
/// localized `bridges.no_link_method`).
///
/// Takes the two `BridgeStatus` fields the rule reads rather than the whole row,
/// so the SPA doesn't pay a serde round-trip per card on every render.
/// `applicableModes` is the count *after* the caller's own platform/capability
/// filter — the string-match half of which is [`bridge_mode_applies`] above.
#[wasm_bindgen(js_name = bridgeLinkBlock)]
pub fn bridge_link_block(
    linked: bool,
    error: Option<String>,
    applicable_modes: u32,
) -> Result<JsValue, JsValue> {
    match fauna_client_bridges::link_block_of(linked, error.as_deref(), applicable_modes as usize) {
        None => Ok(JsValue::NULL),
        Some(fauna_client_bridges::LinkBlock::ProviderError(why)) => {
            crate::rpc::to_js(&serde_json::json!({
                "kind": "provider_error",
                "message": why,
            }))
        }
        Some(fauna_client_bridges::LinkBlock::NoApplicableMode) => {
            crate::rpc::to_js(&serde_json::json!({ "kind": "no_applicable_mode" }))
        }
    }
}

/// `fauna_client_mail_settings::member_status_label` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — the canonical
/// two-arm Subscribed/Unsubscribed label map (`mail-list-members-list-item-status`)
/// that linux/windows/android already consume, so web stops hand-rolling its own
/// `memberStatusBadge`. `status` is the serde variant-name string
/// (`"Subscribed"` / `"Unsubscribed"`) the snapshot already carries.
#[wasm_bindgen(js_name = memberStatusLabel)]
pub fn member_status_label(status: JsValue) -> Result<JsValue, JsValue> {
    let status: fauna_client_mail_settings::MemberStatus = crate::rpc::from_js(status)?;
    crate::rpc::to_js(&fauna_client_mail_settings::member_status_label(status))
}

/// `fauna_client_mail_settings::alias_kind_badge` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — the canonical
/// seven-arm alias-kind badge map (`mail-aliases-list-item-kind`) linux already
/// consumes, so web stops hand-rolling its own `kindBadge`. `kind` is the serde
/// variant-name string (`"Exact"` / `"Wildcard"` / …) the aliases snapshot carries.
#[wasm_bindgen(js_name = aliasKindBadge)]
pub fn alias_kind_badge(kind: JsValue) -> Result<JsValue, JsValue> {
    let kind: fauna_client_mail_settings::AliasKind = crate::rpc::from_js(kind)?;
    crate::rpc::to_js(&fauna_client_mail_settings::alias_kind_badge(kind))
}

/// `fauna_client_mail_settings::alias_hits_label` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — the
/// `mail-aliases-list-item-hits` count text (`mail_aliases.hits` /
/// `mail_aliases.hits_with_last`), so web stops hand-rolling the
/// `"{n} hits · last {date}"` literal (mirrors `aliasKindBadge`). `hit_count` is
/// an `f64` (web's `number`, cast to `u64`). The last-hit `date` is passed in
/// **pre-formatted** — the calendar date is a local-tz concern web keeps in its
/// JS locale date formatter; only the surrounding template is shared. See
/// mail-aliases.md § Aliases UX.
#[wasm_bindgen(js_name = aliasHitsLabel)]
pub fn alias_hits_label(hit_count: f64, last_hit_date: Option<String>) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_mail_settings::alias_hits_label(
        hit_count as u64,
        last_hit_date,
    ))
}

/// `fauna_client_mail_settings::training_label_badge` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — the canonical
/// Spam/Ham training-label map (`mail-spam-training-history-list-item-label`)
/// linux already consumes, so web stops hand-rolling its own `labelBadge`.
/// `label` is the serde variant-name string (`"Spam"` / `"Ham"`).
#[wasm_bindgen(js_name = trainingLabelBadge)]
pub fn training_label_badge(label: JsValue) -> Result<JsValue, JsValue> {
    let label: fauna_client_mail_settings::TrainingLabel = crate::rpc::from_js(label)?;
    crate::rpc::to_js(&fauna_client_mail_settings::training_label_badge(label))
}

/// `fauna_client_mail_settings::training_source_badge` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — the canonical
/// training-source map (`mail-spam-training-history-list-item-source`)
/// linux already consumes, so web stops hand-rolling its own `sourceBadge`.
/// `source` is the serde variant-name string (`"ExplicitButton"` / …).
#[wasm_bindgen(js_name = trainingSourceBadge)]
pub fn training_source_badge(source: JsValue) -> Result<JsValue, JsValue> {
    let source: fauna_client_mail_settings::TrainingSource = crate::rpc::from_js(source)?;
    crate::rpc::to_js(&fauna_client_mail_settings::training_source_badge(source))
}

/// `fauna_client_mail_settings::export_format_label` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — the canonical
/// mbox/Maildir++/EML-zip export-format map (the `mail-export` wizard's confirm /
/// done summary) linux already consumes, so web stops hand-rolling its own
/// `formatLabel`. `format` is the serde variant-name string (`"Mbox"` / …).
#[wasm_bindgen(js_name = exportFormatLabel)]
pub fn export_format_label(format: JsValue) -> Result<JsValue, JsValue> {
    let format: fauna_client_mail_settings::ExportFormat = crate::rpc::from_js(format)?;
    crate::rpc::to_js(&fauna_client_mail_settings::export_format_label(format))
}

/// `fauna_client_mail_settings::import_source_kind_label` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — the canonical
/// Gmail / Outlook / iCloud / Generic map the `mail-import` wizard's source
/// picker and confirm summary read. Exported for the same reason its export
/// twin was: so a seventh app cannot hand-roll a sixth copy of the same
/// four-arm map. `kind` is the serde variant-name string (`"Gmail"` / …).
#[wasm_bindgen(js_name = importSourceKindLabel)]
pub fn import_source_kind_label(kind: JsValue) -> Result<JsValue, JsValue> {
    let kind: fauna_client_mail_settings::ImportSourceKind = crate::rpc::from_js(kind)?;
    crate::rpc::to_js(&fauna_client_mail_settings::import_source_kind_label(kind))
}

/// `fauna_client_mail_settings::import_tls_mode_label` → a `LocalizedText` —
/// the `mail-import-source-tls-mode` picker's two options (implicit / STARTTLS),
/// resolved the same way. `mode` is the serde variant-name string
/// (`"Implicit"` / `"StartTls"`).
#[wasm_bindgen(js_name = importTlsModeLabel)]
pub fn import_tls_mode_label(mode: JsValue) -> Result<JsValue, JsValue> {
    let mode: fauna_client_mail_settings::ImportTlsMode = crate::rpc::from_js(mode)?;
    crate::rpc::to_js(&fauna_client_mail_settings::import_tls_mode_label(mode))
}

/// `fauna_client_mail_settings::connect_actions` → the ordered `MailImportAction`
/// list the wizard's Source step commits then dials with: whichever of
/// `SetHost`/`SetPort` `kind` shows, always `SetUsername`+`SetPassword`, then
/// `Connect` — tui's and linux's shared builder (`docs/goal/behavior/mailbox-migration.md`
/// § Wizard steps step 1), web's own TS port of which named tui's as its model
/// without ever calling it. Each returned action serializes to the same shape
/// `WasmMailImportMachine::dispatch` already accepts (`{ SetHost: { value } }` /
/// the `"Connect"` unit string) — the caller loops `machine.dispatch(action)`
/// over the result in order. `kind` is the serde variant-name string
/// (`"Gmail"` / …); an unparseable `port` is silently dropped, same as the
/// native apps. `password` crosses the wasm boundary as plain text — the
/// dispatched `SetPassword` action is what the machine itself seals.
#[wasm_bindgen(js_name = mailImportConnectActions)]
pub fn mail_import_connect_actions(
    kind: JsValue,
    host: &str,
    port: &str,
    username: &str,
    password: String,
) -> Result<JsValue, JsValue> {
    let kind: fauna_client_mail_settings::ImportSourceKind = crate::rpc::from_js(kind)?;
    crate::rpc::to_js(&fauna_client_mail_settings::connect_actions(
        kind,
        host,
        port,
        username,
        password.into(),
    ))
}

/// `fauna_client_mail_settings::credential_kind_badge` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — the canonical
/// two-arm Password/Bearer label map (`mail-settings-credential-item-type`) the
/// native apps already consume, so web stops hard-coding the English
/// `'Password'`/`'Bearer token'` strings (it had bypassed i18n entirely). `kind`
/// is the serde variant-name string (`"Plain"` / `"OAuthBearer"`) the credential
/// row already carries. See `docs/goal/behavior/mail-credentials.md`.
#[wasm_bindgen(js_name = credentialKindBadge)]
pub fn credential_kind_badge(kind: JsValue) -> Result<JsValue, JsValue> {
    let kind: fauna_client_mail_settings::CredentialKind = crate::rpc::from_js(kind)?;
    crate::rpc::to_js(&fauna_client_mail_settings::credential_kind_badge(kind))
}

/// `fauna_client_mail_settings::settings_status_label` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — the canonical
/// `mail-settings-status-indicator` decision (`Syncing` → `status_syncing`,
/// `RotationInProgress` → `status_rotation({count})`, `Idle` → `status_enabled`
/// when mail is on else `status_disabled`) the native apps consume via UniFFI,
/// so web shares it instead of its own `statusLabel` ternary. `status` is the
/// snapshot's serde `SettingsStatus` (`"Idle"`/`"Syncing"`/`{ RotationInProgress:
/// { credentials_remaining } }`); it is `undefined`/`null` before the snapshot
/// hydrates → treated as `Idle` (with `enabled == false` that reads
/// `status_disabled`, matching the pre-hydrate fallthrough). See
/// `docs/goal/ui/mail-settings.md` § the status indicator.
#[wasm_bindgen(js_name = settingsStatusLabel)]
pub fn settings_status_label(status: JsValue, enabled: bool) -> Result<JsValue, JsValue> {
    let status: fauna_client_mail_settings::SettingsStatus =
        if status.is_undefined() || status.is_null() {
            fauna_client_mail_settings::SettingsStatus::Idle
        } else {
            crate::rpc::from_js(status)?
        };
    crate::rpc::to_js(&fauna_client_mail_settings::settings_status_label(
        status, enabled,
    ))
}

/// `fauna_client_search::render::clean_snippet` → display plaintext (strips the
/// FTS `<b>` match markers + decodes HTML entities). Web renders the raw snippet
/// today, so adopting this fixes the leaked `<b>` markers. See search.md.
#[wasm_bindgen(js_name = searchCleanSnippet)]
pub fn search_clean_snippet(raw: &str) -> String {
    fauna_client_search::render::clean_snippet(raw)
}

/// The Notifications page's per-row icon: `notif_type` wire string → display
/// emoji. `fauna_core::notification_glyph::notification_type_emoji` over wasm
/// — the shared mapping lifted out of web's own hand-written `notificationIcon()`
/// switch (linux had the identical duplicate; both now delegate). See
/// `docs/goal/behavior/notifications.md` § Where logic lives.
#[wasm_bindgen(js_name = notificationTypeGlyph)]
pub fn notification_type_glyph(notif_type: &str) -> String {
    fauna_core::notification_glyph::notification_type_emoji(notif_type).to_string()
}

/// Where a notification row goes when opened, or `null` for an honestly inert
/// row — `fauna_client_notifications::notification_destination` over wasm
/// (`docs/goal/behavior/notifications.md` § Deep-link destinations).
///
/// `row` is one element of a `notificationsList` reply **as it arrives**, before
/// the SPA renames `notif_type` to `type`. The answer is a tagged object:
/// `{ Post: { post_id } }`, `{ Knock: { sender_id } }`, `"Family"`,
/// `{ External: { url } }` — a page outside Fauna (a bridged Bluesky row's
/// post on bsky.app), which the SPA opens in a new tab through its ordinary
/// external-link path (`safe-url.ts`), never inside the app — or `null`.
///
/// ⚠ Web must not substitute its own `notif_type` switch. The decision is keyed
/// on `source` **then** `notif_type`, because a bridged row reuses the native
/// type vocabulary while its `content_id` is a dedup token — matching on the
/// type alone deep-links every bridged like to a post that cannot exist. That
/// trap is why this is one shared function and not seven
/// (§ Don't do these — "Don't deep-link via per-app routing tables").
#[wasm_bindgen(js_name = notificationDestination)]
pub fn notification_destination(row: JsValue) -> Result<JsValue, JsValue> {
    let item: fauna_client_notifications::notifications::NotifItem =
        serde_wasm_bindgen::from_value(row).map_err(crate::rpc::err_to_js)?;
    serde_wasm_bindgen::to_value(&fauna_client_notifications::notification_destination(&item))
        .map_err(crate::rpc::err_to_js)
}

/// What a notification row says — the localized body, the English `summary`,
/// or the default — `fauna_client_notifications::notification_text` over wasm
/// (`docs/goal/behavior/notifications.md` § Localized body).
///
/// `row` is one element of a `notificationsList` reply **as it arrives**, like
/// [`notification_destination`]'s. The answer is
/// `{ kind: "localized", key, args }` — resolve it with the SPA's own
/// `resolveLocalized` — or `{ kind: "verbatim", text }`, painted as-is.
///
/// ⚠ Web must not paint `body` or `summary` on its own judgement. A body whose
/// key this build's catalog lacks (a newer nest's) must lose to `summary`, or
/// the page paints `notifications.some_future_key` at the user; "is the key
/// known" is this call's to answer, against the catalog every app is generated
/// from.
#[wasm_bindgen(js_name = notificationText)]
pub fn notification_text(row: JsValue) -> Result<JsValue, JsValue> {
    use fauna_client_notifications::NotificationText;

    #[derive(serde::Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum Text {
        Localized {
            key: String,
            args: std::collections::HashMap<String, String>,
        },
        Verbatim {
            text: String,
        },
    }

    let item: fauna_client_notifications::notifications::NotifItem =
        serde_wasm_bindgen::from_value(row).map_err(crate::rpc::err_to_js)?;
    let text = match fauna_client_notifications::notification_text(&item) {
        NotificationText::Localized(t) => Text::Localized {
            key: t.key,
            args: t.args,
        },
        NotificationText::Verbatim(text) => Text::Verbatim { text },
    };
    crate::rpc::to_js(&text)
}

/// The conversations rail / feed badge source icon: glyph **id** (the lowercase
/// serde string web reads off `thread.glyph` / `badge.glyph`) → display emoji.
/// `fauna_core::source_glyph::source_glyph_emoji` over wasm — the shared map
/// lifted out of web's own hand-written `sourceGlyphEmoji()` switch, which all
/// seven apps had a copy of. See `render-model.md` § Deltas → D5.
#[wasm_bindgen(js_name = sourceGlyphEmoji)]
pub fn source_glyph_emoji(glyph_id: &str) -> String {
    fauna_core::source_glyph::source_glyph_emoji(glyph_id).to_string()
}

// ── Search paging policy ───────────────────────────────────────────────
//
// wasm faces of `fauna_client_search::paging::SearchPaging`, the twins of
// `fauna-ffi`'s `search_paging_*` UniFFI exports. Free functions over the limit
// rather than an exported object: web keeps `resultLimit` in the page's own
// Svelte state, so it hands the limit back on each call.
//
// `f64` at the boundary, NOT `i64` — a wasm-bindgen `i64` parameter arrives as
// a JS `bigint`, and passing an ordinary `number` to it throws a TypeError at
// the call site. The values here are page sizes (≤ 100), so f64 is exact.
//
// See `docs/goal/ui/search.md` § Where logic lives.

/// `SearchPaging::initial().limit()` — the page size a fresh query asks for,
/// and what submit / type-filter change / cancel resets to.
#[wasm_bindgen(js_name = searchPagingInitialLimit)]
pub fn search_paging_initial_limit() -> f64 {
    fauna_client_search::paging::SearchPaging::initial().limit() as f64
}

/// `SearchPaging::load_more` — the page size to request after a "load more"
/// click. Saturates at the nest's ceiling, so the SPA can never ask for a page
/// the nest silently truncates.
#[wasm_bindgen(js_name = searchPagingLoadMore)]
pub fn search_paging_load_more(current_limit: f64) -> f64 {
    let mut p = fauna_client_search::paging::SearchPaging::at_limit(current_limit as i64);
    p.load_more();
    p.limit() as f64
}

/// `SearchPaging::has_more` — whether `search-load-more-button` should show.
/// False for a partial page AND at the nest's ceiling.
#[wasm_bindgen(js_name = searchPagingHasMore)]
pub fn search_paging_has_more(current_limit: f64, result_count: f64) -> bool {
    fauna_client_search::paging::SearchPaging::at_limit(current_limit as i64)
        .has_more(result_count.max(0.0) as usize)
}

/// `fauna_client_search::kind::TYPE_FILTER_OPTIONS` — the `search-type-filter`
/// option tokens, in render order. `"all"` (`TYPE_FILTER_ALL`) is the
/// client-side sentinel meaning "no filter"; every other token is a nest
/// `content_type` verbatim. Shared so the SPA's dropdown can never drift from
/// the mapping the manager applies to both search backends.
#[wasm_bindgen(js_name = searchTypeFilterOptions)]
pub fn search_type_filter_options() -> Vec<String> {
    fauna_client_search::TYPE_FILTER_OPTIONS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// `fauna_client_search::type_filter_label` → a `LocalizedText` `{ key, args }`
/// the SPA resolves through `L()` — what a `search-type-filter` option is
/// *called*, the companion to `searchTypeFilterOptions`' *which tokens exist*.
///
/// An option is labelled with the very badge its rows carry, so the dropdown
/// and its own results agree by construction; an unrecognised token surfaces
/// raw rather than as a second entry reading "All", which is what web's own
/// closed `typeFilterLabel` match used to do.
#[wasm_bindgen(js_name = searchTypeFilterLabel)]
pub fn search_type_filter_label(token: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_search::type_filter_label(token))
}

/// wasm face of `fauna_client_caldav::calendar_is_displayed`, the twin of
/// `fauna-ffi`'s export (events.md § Where logic lives → *"Which calendars
/// display"*): does an event on `calendar_id` belong on the Events page right
/// now? The whole page-scope composition in one call — the staleness-resolved
/// `calendar-item` selection wins outright (visibility never applies to a
/// selection); with no live selection the `calendar-visibility` toggles filter
/// the union, an **empty** visible-set meaning "no filter" (the full union),
/// never "hide everything". Pass the *raw* stored selection — the staleness
/// rule runs inside.
#[wasm_bindgen(js_name = calendarIsDisplayed)]
pub fn calendar_is_displayed(
    selected: Option<String>,
    existing_ids: Vec<String>,
    visible_calendars: Vec<String>,
    calendar_id: String,
) -> bool {
    fauna_client_caldav::calendar_is_displayed(
        selected.as_deref(),
        &existing_ids,
        &visible_calendars,
        &calendar_id,
    )
}

/// `fauna_protocol::email::encode_filter_rule` → the externally-tagged
/// `EmailFilterRule` JS object (`{ SenderIs: { address } }`, …) for a
/// create-filter dialog's `(kind, value)` pair, so the web filter form shares
/// the native apps' encoder instead of re-deriving the dropdown→variant map.
/// Throws on an unknown kind. See `docs/goal/behavior/smtp-server.md`
/// § Email filter rules.
#[wasm_bindgen(js_name = encodeEmailFilterRule)]
pub fn encode_email_filter_rule(kind: &str, value: &str) -> Result<JsValue, JsValue> {
    let rule =
        fauna_protocol::email::encode_filter_rule(kind, value).map_err(crate::rpc::err_to_js)?;
    crate::rpc::to_js(&rule)
}

/// `fauna_protocol::email::encode_filter_action` over the shared filter form's
/// action inputs — a `FilterActionInputs` object (`{ kind, reject_reason,
/// forward_address, keep_local_copy }`, every field optional) → the
/// `EmailFilterAction` JS value. A `Forward` destination is checked with the
/// nest's own rule-path predicate; an unknown tag or invalid destination throws.
#[wasm_bindgen(js_name = encodeEmailFilterActionInputs)]
pub fn encode_email_filter_action_inputs(inputs: JsValue) -> Result<JsValue, JsValue> {
    let inputs: fauna_protocol::email::FilterActionInputs = crate::rpc::from_js(inputs)?;
    let action =
        fauna_protocol::email::encode_filter_action(&inputs).map_err(crate::rpc::err_to_js)?;
    crate::rpc::to_js(&action)
}

/// `fauna_protocol::email::describe_filter_rule` — the reverse of
/// `encodeEmailFilterRule`: the create-dialog `[kind, value]` pair for a
/// stored `EmailFilterRule`, so an edit form can pre-populate the same
/// rule-type dropdown + value field the create form uses. Returns `null` for
/// the richer variants no dialog collects (`HeaderContains`,
/// `SpamScoreAtLeast`) — a decode gap here means "don't offer edit," not a
/// call failure, so it returns `null` rather than throwing.
#[wasm_bindgen(js_name = describeEmailFilterRule)]
pub fn describe_email_filter_rule(rule: JsValue) -> Result<JsValue, JsValue> {
    let rule: fauna_protocol::email::EmailFilterRule = crate::rpc::from_js(rule)?;
    match fauna_protocol::email::describe_filter_rule(&rule) {
        Some((kind, value)) => crate::rpc::to_js(&(kind, value)),
        None => Ok(JsValue::NULL),
    }
}

/// `fauna_protocol::email::describe_filter_action` — the reverse of
/// `encodeEmailFilterActionInputs`: the `FilterActionInputs` object that
/// reproduces a stored `EmailFilterAction` (a `Forward`'s destination and copy
/// mode included), or `null` for the richer variants no form collects.
#[wasm_bindgen(js_name = describeEmailFilterActionInputs)]
pub fn describe_email_filter_action_inputs(action: JsValue) -> Result<JsValue, JsValue> {
    let action: fauna_protocol::email::EmailFilterAction = crate::rpc::from_js(action)?;
    match fauna_protocol::email::describe_filter_action(&action) {
        Some(inputs) => crate::rpc::to_js(&inputs),
        None => Ok(JsValue::NULL),
    }
}

/// `fauna_protocol::email::filter_action_label` — the `filter-action`
/// list-row badge label for a stored `EmailFilterAction`, as a
/// `LocalizedText` `{ key, args }` the SPA resolves via `resolveLocalized`.
/// Unlike `describeEmailFilterActionInputs` (`null` for the variants no form
/// collects, which only gates *editability*), this always resolves.
#[wasm_bindgen(js_name = emailFilterActionLabel)]
pub fn email_filter_action_label(action: JsValue) -> Result<JsValue, JsValue> {
    let action: fauna_protocol::email::EmailFilterAction = crate::rpc::from_js(action)?;
    crate::rpc::to_js(&fauna_protocol::email::filter_action_label(&action))
}

/// `fauna_protocol::email::filter_is_editable_for` — whether a stored filter
/// can open a form covering `action_kinds`; a form that collects the Forward
/// inputs passes every supported kind.
#[wasm_bindgen(js_name = filterIsEditableFor)]
pub fn filter_is_editable_for(filter: JsValue, action_kinds: Vec<String>) -> Result<bool, JsValue> {
    let filter: fauna_protocol::email::EmailFilter = crate::rpc::from_js(filter)?;
    let kinds: Vec<&str> = action_kinds.iter().map(String::as_str).collect();
    Ok(fauna_protocol::email::filter_is_editable_for(
        &filter.rules,
        &filter.action,
        &kinds,
    ))
}

/// `fauna_protocol::spam::spam_threshold_band` → the `aggressive`/`moderate`/
/// `permissive` i18n key for a `0.0–1.0` spam-threshold slider value, so the web
/// Settings page shares the native apps' label buckets instead of re-deriving
/// them inline. The caller maps the key onto `t.status.spam[key]`. See
/// `docs/goal/ui/settings.md` § Spam threshold slider labels.
#[wasm_bindgen(js_name = spamThresholdBand)]
pub fn spam_threshold_band(threshold: f64) -> String {
    let per_mille = fauna_protocol::spam::probability_to_per_mille(threshold);
    fauna_protocol::spam::spam_threshold_band(per_mille)
        .key()
        .to_string()
}

/// `fauna_protocol::spam::probability_to_per_mille` — the shared `0.0–1.0`
/// probability → per-mille `u16` conversion (clamped + rounded) that the nest,
/// wasm web, and native apps all share instead of each hand-rolling
/// `* 1000`. See `docs/goal/ui/settings.md` § Spam threshold slider labels.
#[wasm_bindgen(js_name = probabilityToPerMille)]
pub fn probability_to_per_mille(probability: f64) -> u16 {
    fauna_protocol::spam::probability_to_per_mille(probability)
}

/// The four `inbox-mode-*` wire tokens in the canonical button order, read
/// straight off the shared `fauna_protocol::contacts::INBOX_MODES` table
/// (`docs/goal/ui/settings.md` § Privacy sub-page item 7).
///
/// **Derived, not re-listed.** This door used to spell the four variants again
/// under a comment promising it matched `INBOX_MODES`' order — the same
/// unchecked "matches the other exactly" promise that table was introduced to
/// retire for tui and linux, reintroduced one layer down. Reading the table
/// makes the agreement structural rather than promised: a reorder there
/// reorders web with it, and there is no second list left to drift.
#[wasm_bindgen(js_name = inboxModeValues)]
pub fn inbox_mode_values() -> Vec<String> {
    fauna_protocol::contacts::INBOX_MODES
        .iter()
        .map(|(token, _label)| (*token).to_string())
        .collect()
}

/// `fauna_protocol::handle::validate_handle` → the canonical handle-format
/// error message for a malformed handle, or `None` when valid. The web Settings
/// change-handle form calls this pre-submit for the SAME instant client-side
/// feedback linux validates natively and apple/windows/android get via the
/// UniFFI twin `fauna_ffi::handle::validate_handle` (priority #2/#3) — one
/// validator for the nest + every app. A *taken* handle stays
/// server-authoritative (surfaced from the `fauna.profile.handle.change` reply).
/// See `docs/goal/ui/settings.md` § Where logic lives → Handle change.
#[wasm_bindgen(js_name = validateHandle)]
pub fn validate_handle(handle: &str) -> Option<String> {
    fauna_protocol::handle::validate_handle(handle)
        .err()
        .map(|m| m.to_string())
}

/// `fauna_protocol::nostr_relay::relay_url_error` → the `LocalizedText` the
/// Nostr settings "Add relay" control shows for a relay URL the user may not
/// add (not `wss://`/`ws://` with a host, or a private-network address —
/// `nest/network-exposure.md` § Rulings F7), or `undefined` when it is
/// acceptable — so web never hand-rolls the check (priority #1/#2; the UniFFI
/// twin is `fauna_ffi::nostr_relay::relay_url_error`). See
/// `docs/goal/ui/nostr.md`.
#[wasm_bindgen(js_name = relayUrlError)]
pub fn relay_url_error(url: &str) -> Result<JsValue, JsValue> {
    match fauna_protocol::nostr_relay::relay_url_error(url) {
        Some(text) => crate::rpc::to_js(&text),
        None => Ok(JsValue::UNDEFINED),
    }
}

/// `fauna_protocol::pending_actions::describe_pending_action` → what a
/// scheduled action will do, as one sentence (`pending-action-description`'s
/// `ui.yaml` contract) — shared with tui/linux so an unrecognized
/// `action_type` (client/nest skew) paints the same raw fallback everywhere.
/// See `docs/goal/ui/settings.md` § Pending actions.
#[wasm_bindgen(js_name = describePendingAction)]
pub fn describe_pending_action(action_type: &str, target: Option<String>) -> String {
    fauna_protocol::pending_actions::describe_pending_action(action_type, target.as_deref())
}

/// `fauna_protocol::nostr_relay::trimmed_relay_input` → the trimmed relay
/// input, or `undefined` when that's empty. See `docs/goal/ui/nostr.md`.
#[wasm_bindgen(js_name = trimmedRelayInput)]
pub fn trimmed_relay_input(input: &str) -> Option<String> {
    fauna_protocol::nostr_relay::trimmed_relay_input(input)
}

/// `fauna_protocol::nostr_relay::relay_list_appending` → `existing` with
/// `url` appended, or `undefined` when `url` is already present. See
/// `docs/goal/ui/nostr.md`.
#[wasm_bindgen(js_name = relayListAppending)]
pub fn relay_list_appending(existing: Vec<String>, url: &str) -> Option<Vec<String>> {
    fauna_protocol::nostr_relay::relay_list_appending(&existing, url)
}

/// `fauna_core::resolve::classify_recipient` → a flat `[kind, actorId, user, domain]`
/// array classifying a typed "compose / find-user" recipient input. `kind` is
/// `"actor_id"` (slot 1 = lowercased 64-hex), `"handle"` (slots 2,3 = `user`,`domain`),
/// or `"invalid"`. Web's `resolve.ts` `parseRecipient` shares this with android/linux
/// instead of re-implementing the actor-id check + handle split (priority #2/#4).
#[wasm_bindgen(js_name = classifyRecipient)]
pub fn classify_recipient(input: &str) -> Vec<String> {
    fauna_core::resolve::classify_recipient(input).into_parts()
}

/// `fauna_conversations::TypedAddress::display` → the canonical per-rail display
/// string (Fauna→handle, Email→address, Bluesky→handle, Nostr→npub, ActivityPub→acct)
/// for a raw `TypedAddress`. Web's `conversations/+page.svelte` `addrDisplay` shares
/// this one switch — the wasm twin of the native FFI `typed_address_display`
/// (windows/android/apple) and linux's direct `.display()` call — instead of
/// re-implementing the variant→string map in TS (priority #2/#4). `addr` is the
/// externally-tagged serde object the conversations snapshot serializes
/// (`{Email:{email_address}}`, `{Fauna:{handle, actor_id}}`, …); the `actor_id`
/// `[u8;32]` round-trips through `json_compatible()` (the snapshot serializer).
/// See `conversations.md` § Where logic lives.
#[wasm_bindgen(js_name = typedAddressDisplay)]
pub fn typed_address_display(addr: JsValue) -> Result<String, JsValue> {
    let addr: fauna_conversations::TypedAddress =
        serde_wasm_bindgen::from_value(addr).map_err(crate::rpc::err_to_js)?;
    Ok(addr.display())
}

/// `fauna_core::markdown::markdown_to_html` → the canonical HTML for a NIP-23 article
/// body, so web's `markdown.ts` shares the one parser the native apps also render from
/// (priority #1/#2/#4) instead of re-implementing the Markdown subset. Byte-identical to
/// the old web output for non-nested inline (all real article content).
#[wasm_bindgen(js_name = markdownToHtml)]
pub fn markdown_to_html(md: &str) -> String {
    fauna_core::markdown::markdown_to_html(md)
}

/// `fauna_core::markdown::markdown_to_html_blocked` → HTML for an **untrusted inbound**
/// body (a mail / DM `BodyFormat::Markdown` body) with remote `![]()` images rendered
/// as no-`src` placeholders (`data-remote-src`, `class="blocked-remote-image"`) — the
/// privacy posture (never auto-fetch). The web conversations bubble reveals them per
/// message by copying `data-remote-src` → `src` when the user clicks
/// `load-remote-content-button`.
#[wasm_bindgen(js_name = markdownToHtmlBlocked)]
pub fn markdown_to_html_blocked(md: &str) -> String {
    fauna_core::markdown::markdown_to_html_blocked(md)
}

/// `fauna_core::markdown::count_remote_images` → how many remote `![]()` images a body
/// has, so the web bubble shows the `load-remote-content-button` only when ≥1 is blocked.
#[wasm_bindgen(js_name = countRemoteImages)]
pub fn count_remote_images(md: &str) -> usize {
    fauna_core::markdown::count_remote_images(md)
}

/// `fauna_core::render::markdown_to_document` → the shared semantic
/// [`RenderDocument`](fauna_core::render::RenderDocument) for a body: the typed
/// block/inline/embed tree the web app will paint, with remote `![]()` images promoted
/// to blocked-by-default `RemoteImage` blocks in body order (render-model.md). Returns the
/// serde JSON shape (externally-tagged enums); no client renders it yet (P1 — the web
/// conversations/feed legs adopt it in P2/P3).
#[wasm_bindgen(js_name = markdownToDocument)]
pub fn markdown_to_document(md: &str) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_core::render::markdown_to_document(md))
        .map_err(crate::rpc::err_to_js)
}

/// `fauna_core::render::RenderDocument::has_blocked_remote_images` → whether the document carries
/// at least one **un-revealed** remote image (a body [`RenderBlock::RemoteImage`], or a `Resolved`
/// link-preview og:image at any nesting depth), i.e. the post/message `load-remote-content-button`
/// should show (render-model.md § D3, § D4 the og:image twin). The **browser twin** of the UniFFI
/// `render_document_has_blocked_remote_images` face (linux native / windows UniFFI): web gates the
/// button on this ONE shared predicate instead of re-walking the block tree in TS, so a new
/// blocked-content arm (the D4 og:image, the D7a task-list recursion) can't drift per-app and a
/// top-level-only walk can't silently miss a nested embed. `doc` is the `RenderDocument` the manager
/// projects — the serde shape `markdownToDocument` returns / a `MessageSnapshot.document` carries.
#[wasm_bindgen(js_name = renderDocumentHasBlockedRemoteImages)]
pub fn render_document_has_blocked_remote_images(doc: JsValue) -> Result<bool, JsValue> {
    let doc: fauna_core::render::RenderDocument =
        serde_wasm_bindgen::from_value(doc).map_err(crate::rpc::err_to_js)?;
    Ok(doc.has_blocked_remote_images())
}

/// `fauna_core::render::RenderDocument::first_image_hash` → the content hash of the
/// first trusted embedded image, so web can paint the `post-image`/media bytes through
/// its own blob loader instead of re-walking the block tree itself. The browser twin of
/// the UniFFI `render_document_first_image_hash` face.
#[wasm_bindgen(js_name = renderDocumentFirstImageHash)]
pub fn render_document_first_image_hash(doc: JsValue) -> Result<Option<String>, JsValue> {
    let doc: fauna_core::render::RenderDocument =
        serde_wasm_bindgen::from_value(doc).map_err(crate::rpc::err_to_js)?;
    Ok(doc.first_image_hash().map(str::to_string))
}

/// `fauna_core::render::RenderDocument::first_video_hash` → the content hash of the first
/// trusted embedded **video**, the twin of [`render_document_first_image_hash`] and the
/// accessor web paints `video-thumbnail` from. The browser twin of the UniFFI
/// `render_document_first_video_hash` face.
///
/// Before the typed `RenderBlock::Video` variant existed, web could only find this by calling
/// `decode_post` a second time and branching on `media_type` in app code — the workaround this
/// export retires (render-model.md § Implementation status today).
#[wasm_bindgen(js_name = renderDocumentFirstVideoHash)]
pub fn render_document_first_video_hash(doc: JsValue) -> Result<Option<String>, JsValue> {
    let doc: fauna_core::render::RenderDocument =
        serde_wasm_bindgen::from_value(doc).map_err(crate::rpc::err_to_js)?;
    Ok(doc.first_video_hash().map(str::to_string))
}

/// `fauna_core::render::RenderDocument::media_blocks` → every trusted media block (`Image` /
/// `Video`) in body order, for a page that paints them ALL rather than just the first.
///
/// Web is that page today — it has always rendered every attachment, which is the richest
/// existing pattern (priority #4) and the reason the shared fold is multi-item. The
/// single-element painters use `renderDocumentFirstImageHash` / `…FirstVideoHash` instead,
/// matching their non-indexed `post-image` / `video-thumbnail` elements.
#[wasm_bindgen(js_name = renderDocumentMediaBlocks)]
pub fn render_document_media_blocks(doc: JsValue) -> Result<JsValue, JsValue> {
    let doc: fauna_core::render::RenderDocument =
        serde_wasm_bindgen::from_value(doc).map_err(crate::rpc::err_to_js)?;
    serde_wasm_bindgen::to_value(&doc.media_blocks()).map_err(crate::rpc::err_to_js)
}

/// `fauna_core::render::RenderDocument::proxied_images` → every `ProxiedImage` block in body
/// order as owned `{path, alt}` records: a bridged post's own pictures, each a nest-relative
/// path web fetches with its authenticated `fetch` and paints as an object URL in the
/// `post-image` slot, immediately (render-model.md § D6c). The browser twin of the UniFFI
/// `render_document_proxied_images` face.
#[wasm_bindgen(js_name = renderDocumentProxiedImages)]
pub fn render_document_proxied_images(doc: JsValue) -> Result<JsValue, JsValue> {
    let doc: fauna_core::render::RenderDocument =
        serde_wasm_bindgen::from_value(doc).map_err(crate::rpc::err_to_js)?;
    let owned: Vec<fauna_core::render::ProxiedImageRefOwned> =
        doc.proxied_images().into_iter().map(Into::into).collect();
    serde_wasm_bindgen::to_value(&owned).map_err(crate::rpc::err_to_js)
}

/// `fauna_core::render::RenderDocument::proxied_videos` → every `ProxiedVideo` block in body
/// order as owned `{path, alt}` records: a bridged post's own videos, each a nest-relative
/// path web paints in its `video-thumbnail` slot without byte-loading it (render-model.md
/// § D6c → *Proxied video*). The browser twin of the UniFFI `render_document_proxied_videos`
/// face.
#[wasm_bindgen(js_name = renderDocumentProxiedVideos)]
pub fn render_document_proxied_videos(doc: JsValue) -> Result<JsValue, JsValue> {
    let doc: fauna_core::render::RenderDocument =
        serde_wasm_bindgen::from_value(doc).map_err(crate::rpc::err_to_js)?;
    let owned: Vec<fauna_core::render::ProxiedVideoRefOwned> =
        doc.proxied_videos().into_iter().map(Into::into).collect();
    serde_wasm_bindgen::to_value(&owned).map_err(crate::rpc::err_to_js)
}

/// `fauna_core::render::RenderDocument::quoted_post` → the folded feed quote-post embed,
/// if the manager has resolved one. Returns the owned `QuotedPostEmbedOwned` serde shape
/// (the borrowed `QuotedPostEmbed` carries a Rust lifetime that can't cross wasm). The
/// browser twin of the UniFFI `render_document_quoted_post` face.
#[wasm_bindgen(js_name = renderDocumentQuotedPost)]
pub fn render_document_quoted_post(doc: JsValue) -> Result<JsValue, JsValue> {
    let doc: fauna_core::render::RenderDocument =
        serde_wasm_bindgen::from_value(doc).map_err(crate::rpc::err_to_js)?;
    let owned: Option<fauna_core::render::QuotedPostEmbedOwned> = doc.quoted_post().map(Into::into);
    serde_wasm_bindgen::to_value(&owned).map_err(crate::rpc::err_to_js)
}

/// `fauna_core::render::RenderDocument::resolving_link_preview_urls` → the urls of link
/// previews still resolving, in body order, so web can fire the resolve for each without
/// re-walking the block tree itself. The browser twin of the UniFFI
/// `render_document_resolving_link_preview_urls` face.
#[wasm_bindgen(js_name = renderDocumentResolvingLinkPreviewUrls)]
pub fn render_document_resolving_link_preview_urls(doc: JsValue) -> Result<Vec<String>, JsValue> {
    let doc: fauna_core::render::RenderDocument =
        serde_wasm_bindgen::from_value(doc).map_err(crate::rpc::err_to_js)?;
    Ok(doc
        .resolving_link_preview_urls()
        .into_iter()
        .map(str::to_string)
        .collect())
}

/// `fauna_core::render::RenderDocument::resolved_link_previews` → the resolved link
/// previews, in body order, as owned `ResolvedLinkPreviewOwned` serde shapes. The browser
/// twin of the UniFFI `render_document_resolved_link_previews` face.
#[wasm_bindgen(js_name = renderDocumentResolvedLinkPreviews)]
pub fn render_document_resolved_link_previews(doc: JsValue) -> Result<JsValue, JsValue> {
    let doc: fauna_core::render::RenderDocument =
        serde_wasm_bindgen::from_value(doc).map_err(crate::rpc::err_to_js)?;
    let owned: Vec<fauna_core::render::ResolvedLinkPreviewOwned> = doc
        .resolved_link_previews()
        .into_iter()
        .map(Into::into)
        .collect();
    serde_wasm_bindgen::to_value(&owned).map_err(crate::rpc::err_to_js)
}

/// `fauna_core::render::RenderDocument::link_previews` → every link preview in body
/// order with its state name (`{url, state}`, state `resolving` / `resolved` /
/// `failed`), whatever the state — what web's e2e state dump publishes as
/// `data.feed.posts[].link_previews` (render-model.md § D4). The browser twin of the
/// UniFFI `render_document_link_previews` face.
#[wasm_bindgen(js_name = renderDocumentLinkPreviews)]
pub fn render_document_link_previews(doc: JsValue) -> Result<JsValue, JsValue> {
    let doc: fauna_core::render::RenderDocument =
        serde_wasm_bindgen::from_value(doc).map_err(crate::rpc::err_to_js)?;
    let owned: Vec<fauna_core::render::LinkPreviewStateOwned> =
        doc.link_previews().into_iter().map(Into::into).collect();
    serde_wasm_bindgen::to_value(&owned).map_err(crate::rpc::err_to_js)
}

/// `fauna_core::render::RenderDocument::remote_images` → every [`RenderBlock::RemoteImage`]
/// in body order, each with its own manager-projected `revealed` flag, as owned
/// `RemoteImageRefOwned` serde shapes. The browser twin of the UniFFI
/// `render_document_remote_images` face.
#[wasm_bindgen(js_name = renderDocumentRemoteImages)]
pub fn render_document_remote_images(doc: JsValue) -> Result<JsValue, JsValue> {
    let doc: fauna_core::render::RenderDocument =
        serde_wasm_bindgen::from_value(doc).map_err(crate::rpc::err_to_js)?;
    let owned: Vec<fauna_core::render::RemoteImageRefOwned> =
        doc.remote_images().into_iter().map(Into::into).collect();
    serde_wasm_bindgen::to_value(&owned).map_err(crate::rpc::err_to_js)
}

/// `fauna_core::markdown::decoration_map` → the compose field's inline markdown styling
/// map (styled-content + marker byte ranges over the **raw** source) for the web compose
/// applier (a CodeMirror `Decoration` set). Returns `[{start, end, kind, level}]` — `kind`
/// the snake_case token, `level` the heading level (1–4) when `kind === "heading"`, else 0.
/// The same shared tokenizer feeds `markdownToHtml`, so the editor preview and the sent
/// message never disagree. See `conversations.md` § Compose-field inline markdown styling.
#[wasm_bindgen(js_name = decorationMap)]
pub fn decoration_map(src: &str) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_core::markdown::decoration_map(src))
        .map_err(crate::rpc::err_to_js)
}

/// `fauna_core::markdown::inline_reveal_ranges` → the Notes editor's caret-edge reveal set:
/// the inline-emphasis marker byte ranges to **reveal** (un-hide) for a caret at byte offset
/// `caret`, so a markers-never-shown (Notes) editor can edit the raw `**`/`*`/`` ` ``/`[]()`
/// of the run the caret is in. Returns `[{start, end}]` (UTF-8 byte ranges over the raw
/// source, same space as `decorationMap`); structural block markers are never returned. One
/// shared policy for web + native so they can't drift on which markers reveal (priority #1/#2).
/// See the Notes editor design spec § Inline conceal.
#[wasm_bindgen(js_name = inlineRevealRanges)]
pub fn inline_reveal_ranges(src: &str, caret: usize) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_core::markdown::inline_reveal_ranges(src, caret))
        .map_err(crate::rpc::err_to_js)
}

/// `fauna_core::markdown::compose_show_markers_dim_ranges` → the compose field's "show markers"
/// (dimmed live-preview) mode reveal policy: the marker byte-ranges to DIM (every inline-emphasis
/// or structural marker whose source line differs from the caret's line — markers on the caret's
/// line are revealed, i.e. NOT returned). Returns `[{start, end}]` (UTF-8 byte ranges over the
/// raw source, same space as `decorationMap`). One shared policy for web + native (windows,
/// android, linux already consume it); replaces web's hand-rolled `caretLine`/`lineOfUtf16`
/// marker filter in `markdown-decorations.ts`. See `docs/goal/ui/conversations.md` § the compose
/// "show markers" (dim) mode.
#[wasm_bindgen(js_name = composeShowMarkersDimRanges)]
pub fn compose_show_markers_dim_ranges(src: &str, caret: usize) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_core::markdown::compose_show_markers_dim_ranges(
        src, caret,
    ))
    .map_err(crate::rpc::err_to_js)
}

/// `fauna_core::notes::parse_note` → the Notes editor block model for a markdown buffer: a
/// `{blocks: [{id, kind, depth, checked, level, text}]}` document. The Fork-1 lowering's first
/// step (markdown buffer → blocks); inline markers stay in each block's `text`. See the Notes
/// editor design spec § The block-model sketch.
#[wasm_bindgen(js_name = parseNote)]
pub fn parse_note(md: &str) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_core::notes::parse_note(md)).map_err(crate::rpc::err_to_js)
}

/// `fauna_core::notes::note_line_map` → the per-buffer-line structural projection for a Notes
/// markdown buffer + its `parseNote` blocks: one `{index, start, end, role, block, prefix_from,
/// prefix_to, checkbox_index, ordered_number}` per buffer line, in **byte** offsets (the client
/// remaps to its caret unit — web → CodeMirror UTF-16). This is the shared structural derivation
/// (code folding, checkbox indexing, ordered-list numbering, structural-prefix range) every
/// app's Notes view turns into chrome (bullet / number / checkbox over the hidden prefix) +
/// per-line styling — computed once, never re-derived per client (priority #2). `blocks` is the
/// `blocks` array `parseNote` returns. See the Notes editor design spec § The linchpin.
#[wasm_bindgen(js_name = noteLineMap)]
pub fn note_line_map(value: &str, blocks: JsValue) -> Result<JsValue, JsValue> {
    let blocks: Vec<fauna_core::notes::Block> =
        serde_wasm_bindgen::from_value(blocks).map_err(crate::rpc::err_to_js)?;
    serde_wasm_bindgen::to_value(&fauna_core::notes::note_line_map(value, &blocks))
        .map_err(crate::rpc::err_to_js)
}

/// `fauna_core::notes::caret_to_block_caret` → map a **whole-buffer UTF-8 byte** caret to a shared
/// `{block, offset}` `BlockCaret` (the gesture engine's caret: a block id + a byte offset into
/// that block's `text`), or `null` when it can't be resolved (empty buffer). The block lookup /
/// structural-prefix skip / code-body byte accumulation is shared Rust — web converts CodeMirror's
/// UTF-16 caret to a byte offset first (the only seam web keeps; see `$lib/notes-editor`). `blocks`
/// is the `parseNote` array; the per-line projection is recomputed here from `(value, blocks)`.
#[wasm_bindgen(js_name = caretToBlockCaret)]
pub fn caret_to_block_caret(
    value: &str,
    blocks: JsValue,
    caret_byte: usize,
) -> Result<JsValue, JsValue> {
    let blocks: Vec<fauna_core::notes::Block> =
        serde_wasm_bindgen::from_value(blocks).map_err(crate::rpc::err_to_js)?;
    let line_map = fauna_core::notes::note_line_map(value, &blocks);
    let caret = fauna_core::notes::caret_to_block_caret(&line_map, &blocks, caret_byte);
    serde_wasm_bindgen::to_value(&caret).map_err(crate::rpc::err_to_js)
}

/// `fauna_core::notes::block_caret_to_byte` → the inverse of `caretToBlockCaret`: map a shared
/// `{block, offset}` `BlockCaret` (e.g. the caret a structural gesture returns) back to a
/// **whole-buffer UTF-8 byte** offset against the post-gesture `(value, blocks)`. Web converts the
/// returned byte offset back to a CodeMirror UTF-16 caret at the seam. `blocks` is the `parseNote`
/// array; the per-line projection is recomputed here from `(value, blocks)`.
#[wasm_bindgen(js_name = blockCaretToByte)]
pub fn block_caret_to_byte(value: &str, blocks: JsValue, caret: JsValue) -> Result<usize, JsValue> {
    let blocks: Vec<fauna_core::notes::Block> =
        serde_wasm_bindgen::from_value(blocks).map_err(crate::rpc::err_to_js)?;
    let caret: fauna_core::notes::BlockCaret =
        serde_wasm_bindgen::from_value(caret).map_err(crate::rpc::err_to_js)?;
    let line_map = fauna_core::notes::note_line_map(value, &blocks);
    Ok(fauna_core::notes::block_caret_to_byte(
        &line_map, &blocks, caret,
    ))
}

/// `fauna_core::markdown::compose_decoration_plan` → the compose field's hide-by-default marker
/// treatment for a caret at byte offset `caret`: `{hide: [{start,end}], dim: [{start,end}]}` —
/// inline emphasis markers to CONCEAL (`hide`) vs. markers to show DIMMED (`dim` = structural
/// prefixes off the caret line + inline markers revealed at the caret edge). Markers in neither
/// set are shown plain (a structural prefix on the caret's own line). Content styling is
/// unchanged — the client styles non-marker `decorationMap` kinds as before. One shared policy
/// for web + native.
#[wasm_bindgen(js_name = composeDecorationPlan)]
pub fn compose_decoration_plan(src: &str, caret: usize) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&fauna_core::markdown::compose_decoration_plan(src, caret))
        .map_err(crate::rpc::err_to_js)
}

/// `fauna_core::notes::serialize_note` → the lossless markdown for a Notes block document (the
/// inverse of `parseNote`; the Fork-1 lowering's last step, blocks → markdown buffer). `doc` is
/// the document object `parseNote`/`applyEdits` return.
#[wasm_bindgen(js_name = serializeNote)]
pub fn serialize_note(doc: JsValue) -> Result<String, JsValue> {
    let doc: fauna_core::notes::NoteDocument =
        serde_wasm_bindgen::from_value(doc).map_err(crate::rpc::err_to_js)?;
    Ok(fauna_core::notes::serialize_note(&doc))
}

/// `fauna_core::notes::apply_structural_gesture` → the pure bullets-first gesture engine: given
/// the block document, a `{block, offset}` caret, a snake_case gesture (`newline` / `indent` /
/// `outdent` / `toggle_checkbox` / `backspace_at_start`), and a fresh `new_id`, returns
/// `{edits, caret}`. The client lowers `edits` to substrate ops (Fork-1: `applyEdits` then
/// `serializeNote`). Mirrors `wrapMarkdownSelection`. See the spec § Structural-gesture API.
#[wasm_bindgen(js_name = applyStructuralGesture)]
pub fn apply_structural_gesture(
    doc: JsValue,
    caret: JsValue,
    gesture: JsValue,
    new_id: JsValue,
) -> Result<JsValue, JsValue> {
    let doc: fauna_core::notes::NoteDocument =
        serde_wasm_bindgen::from_value(doc).map_err(crate::rpc::err_to_js)?;
    let caret: fauna_core::notes::BlockCaret =
        serde_wasm_bindgen::from_value(caret).map_err(crate::rpc::err_to_js)?;
    let gesture: fauna_core::notes::StructuralGesture =
        serde_wasm_bindgen::from_value(gesture).map_err(crate::rpc::err_to_js)?;
    let new_id: fauna_core::notes::BlockId =
        serde_wasm_bindgen::from_value(new_id).map_err(crate::rpc::err_to_js)?;
    serde_wasm_bindgen::to_value(&fauna_core::notes::apply_structural_gesture(
        &doc, caret, gesture, new_id,
    ))
    .map_err(crate::rpc::err_to_js)
}

/// `fauna_core::notes::apply_edits` → apply a `BlockEdit` list to a Notes block document (the
/// Fork-1 reference lowering: in-memory block surgery). `doc` + `edits` are the objects from
/// `parseNote` + `applyStructuralGesture`; returns the resulting document.
#[wasm_bindgen(js_name = applyEdits)]
pub fn apply_edits(doc: JsValue, edits: JsValue) -> Result<JsValue, JsValue> {
    let doc: fauna_core::notes::NoteDocument =
        serde_wasm_bindgen::from_value(doc).map_err(crate::rpc::err_to_js)?;
    let edits: Vec<fauna_core::notes::BlockEdit> =
        serde_wasm_bindgen::from_value(edits).map_err(crate::rpc::err_to_js)?;
    serde_wasm_bindgen::to_value(&fauna_core::notes::apply_edits(&doc, &edits))
        .map_err(crate::rpc::err_to_js)
}

/// `fauna_core::markdown::wrap_selection` → wrap a compose-toolbar selection in
/// `prefix`/`suffix` markers, keeping edge whitespace OUTSIDE them so a word-selection's
/// trailing space doesn't yield `*italic *` (which collides with an adjacent `**bold**`
/// into `*italic ***bold**`). Web's `MarkdownToolbar` shares this with the native
/// toolbars instead of re-deriving the wrap in TS (priority #1/#2/#4). Returns
/// `[replacement, before_core, core]` (mirrors `classifyRecipient`'s flat array): the
/// caller splices `replacement` over the selection, then re-selects `core` by shifting
/// the selection start past `before_core`'s length. Empty/all-whitespace selection wraps
/// `placeholder` (`""` ⇒ cursor between the markers).
#[wasm_bindgen(js_name = wrapMarkdownSelection)]
pub fn wrap_markdown_selection(
    selected: &str,
    prefix: &str,
    suffix: &str,
    placeholder: &str,
) -> Vec<String> {
    let w = fauna_core::markdown::wrap_selection(selected, prefix, suffix, placeholder);
    vec![w.replacement, w.before_core, w.core]
}

/// `fauna_core::format::short_id` → the canonical short display form for a long
/// hex id (first 12 chars + `…`). Web's `feed-utils.ts` `shortActor` shares this
/// with the native apps instead of re-deriving `slice(0,12)+'...'`
/// (priority #1/#4). See `docs/goal/behavior/value-formatting.md` § Short id.
#[wasm_bindgen(js_name = shortId)]
pub fn short_id(hex: &str) -> String {
    fauna_core::format::short_id(hex)
}

/// `fauna_core::format::short_nest_id` → the head…tail elision of a long hex
/// `nest_actor_id` for a box-recovery row label (first 8 chars, `…`, last 8;
/// ids of 20 or fewer chars pass through). The browser twin of the native
/// `short_nest_id` UniFFI face, so web's `onboarding/+page.svelte` stops
/// hand-rolling the truncation (priority #2). Distinct from [`short_id`], which
/// keeps only a 12-char prefix. See `docs/goal/behavior/value-formatting.md`
/// § Short nest id.
#[wasm_bindgen(js_name = shortNestId)]
pub fn short_nest_id(id: &str) -> String {
    fauna_core::format::short_nest_id(id)
}

/// `fauna_core::format::account_display_label` → the account-switcher row title:
/// the cached handle when present **and non-empty**, else the canonical
/// [`short_id`] of the actor id. The browser twin of the native
/// `account_display_label` UniFFI face. Web's `settings/[[subpage]]/+page.svelte`
/// had drifted (`handle ?? actor_id.slice(0, 12)` — no ellipsis, and `??` does
/// not treat an empty string as absent) despite a comment claiming it mirrored
/// the Linux reference; routing both through this fn resolves that
/// (priority #1/#4). An absent handle crosses the boundary as JS `undefined`.
/// See `docs/goal/behavior/value-formatting.md` § Account display label.
#[wasm_bindgen(js_name = accountDisplayLabel)]
pub fn account_display_label(handle: Option<String>, actor_id: &str) -> String {
    fauna_core::format::account_display_label(handle.as_deref(), actor_id)
}

/// `fauna_core::format::hex_short` → the short display hex for a **byte** id: the
/// first 4 bytes as 8 lowercase zero-padded hex chars (the backup restore-source
/// label). A plain `string`, so web's `backups/+page.svelte` stops hand-rolling
/// the `slice(0,4).map(toString(16))` truncation (priority #2). `&[u8]` ↔ JS
/// `Uint8Array`. See `docs/goal/ui/backups.md` § Where logic lives.
#[wasm_bindgen(js_name = hexShort)]
pub fn hex_short(bytes: &[u8]) -> String {
    fauna_core::format::hex_short(bytes)
}

/// `fauna_core::format::hex_full` → the full display hex for a **byte** id: every
/// byte as two lowercase zero-padded hex chars — the canonical actor/member-id
/// fallback label shown when no handle is available. A plain `string`, so web's
/// admin surfaces stop hand-rolling the `Array.from(bytes).map(toString(16))`
/// byte→hex in `hex.ts` `actorHex` (priority #2/#4). The full-length sibling of
/// [`hex_short`]. `&[u8]` ↔ JS `Uint8Array`. See
/// `docs/goal/behavior/value-formatting.md` § Hex id display.
#[wasm_bindgen(js_name = hexFull)]
pub fn hex_full(bytes: &[u8]) -> String {
    fauna_core::format::hex_full(bytes)
}

/// `fauna_core::format::url_host` → the display host of a `scheme://host[:port]/…`
/// URL (scheme/port/path dropped), e.g. `"example.com"` for
/// `"https://example.com/article"`; returns the original string when no host can
/// be isolated. The shared label the `link-preview-domain` child renders
/// (render-model.md § D4) — web's `LinkPreviewCard.svelte` was the only client
/// hand-rolling this (`new URL(u).hostname`) instead of consuming the already-FFI-
/// exported `fauna_core::format::url_host` linux/tui/android/apple/windows all
/// call (priority #1/#2/#4). See `docs/goal/behavior/value-formatting.md` § URL
/// host display.
#[wasm_bindgen(js_name = urlHost)]
pub fn url_host(url: &str) -> String {
    fauna_core::format::url_host(url)
}

/// `fauna_core::format::confidence_percent` → the whole-percent display of a
/// moderation classifier's `confidence_per_mille` (`0..=1000`), rounded **half-up**
/// (`920 ‰ → 92 %`, `995 ‰ → 100 %`). The browser twin of the native
/// `confidence_percent` UniFFI face, so web's content-label badge + moderation queue
/// stop hand-rolling the rounding (`.toFixed(0)` / `Math.round(* 100)` — two mutually
/// inconsistent web copies that also drifted from the 5 native apps' half-up rule).
/// Input is per-mille (the dag-cbor wire form, floats forbidden); web quantizes its
/// local-classifier float via `Math.round(confidence * 1000)`. Plain `u32` out — the
/// number is locale-invariant. See `docs/goal/behavior/value-formatting.md`
/// § Confidence percent (priority #1/#2/#4).
#[wasm_bindgen(js_name = confidencePercent)]
pub fn confidence_percent(per_mille: u16) -> u32 {
    fauna_core::format::confidence_percent(per_mille)
}

/// `fauna_core::format::quota_fraction` → a `{used_bytes, max_bytes}` usage pair
/// reduced to a `0.0..=1.0` bar-fill fraction, guarded against `max_bytes <= 0`
/// (returns `0.0` — no divide-by-zero) and clamped to `1.0` when over-quota.
/// Web's `+page.svelte` hand-rolled `used_bytes / max_bytes` with **no**
/// zero-guard, so a `max_bytes == 0` quota row produced a `NaN` bar width — a
/// real bug this closes (priority #1/#2/#4). `used_bytes`/`max_bytes` are web's
/// `number` (the wire's `i64`, cast at the boundary). See
/// `docs/goal/behavior/value-formatting.md` § Quota fraction.
#[wasm_bindgen(js_name = quotaFraction)]
pub fn quota_fraction(used_bytes: f64, max_bytes: f64) -> f64 {
    fauna_core::format::quota_fraction(used_bytes as i64, max_bytes as i64)
}

/// `fauna_core::format::quota_percent` → the whole-percent sibling of
/// [`quota_fraction`], rounded to the nearest percent, for a text label. See
/// `docs/goal/behavior/value-formatting.md` § Quota fraction.
#[wasm_bindgen(js_name = quotaPercent)]
pub fn quota_percent(used_bytes: f64, max_bytes: f64) -> u32 {
    fauna_core::format::quota_percent(used_bytes as i64, max_bytes as i64)
}

/// `fauna_core::format::total_pages` → the total page count for an admin list
/// paginated by (`page_size`, `total` items), ceil-divided and floored to `1`
/// (an empty list still shows `"1 / 1"`, never `"1 / 0"`). Replaces web's
/// hand-rolled `Math.max(1, Math.ceil(total / PAGE_SIZE))` on the admin-users
/// page — the same formula windows/linux/tui each independently hand-rolled
/// too (priority #1/#2/#4). `total`/`page_size` are web's `number` (the wire's
/// `i64`, cast at the boundary). See `docs/goal/behavior/value-formatting.md`
/// § Pagination.
#[wasm_bindgen(js_name = totalPages)]
pub fn total_pages(total: f64, page_size: f64) -> f64 {
    fauna_core::format::total_pages(total as i64, page_size as i64) as f64
}

/// `fauna_core::format::current_page` → the 1-based current page number from a
/// 0-based `offset` into a list paginated by `page_size`. Replaces web's
/// hand-rolled `Math.floor(offset / PAGE_SIZE) + 1`. See [`total_pages`].
#[wasm_bindgen(js_name = currentPage)]
pub fn current_page(offset: f64, page_size: f64) -> f64 {
    fauna_core::format::current_page(offset as i64, page_size as i64) as f64
}

/// `fauna_core::format::next_page_offset` → the offset one page forward, or
/// `undefined` at the last page. Replaces web's hand-rolled
/// `offset + PAGE_SIZE >= total ? undefined : offset + PAGE_SIZE` guard on the
/// admin-users page — the stepper half of the [`total_pages`]/[`current_page`]
/// pagination harvest. See `docs/goal/behavior/value-formatting.md` § Pagination.
#[wasm_bindgen(js_name = nextPageOffset)]
pub fn next_page_offset(offset: f64, total: f64, page_size: f64) -> Option<f64> {
    fauna_core::format::next_page_offset(offset as i64, total as i64, page_size as i64)
        .map(|v| v as f64)
}

/// `fauna_core::format::prev_page_offset` → the offset one page back, or
/// `undefined` at page 1. See [`next_page_offset`].
#[wasm_bindgen(js_name = prevPageOffset)]
pub fn prev_page_offset(offset: f64, page_size: f64) -> Option<f64> {
    fauna_core::format::prev_page_offset(offset as i64, page_size as i64).map(|v| v as f64)
}

/// `fauna_client_web::subdomain_view` → `{ enabled, url?, disabled_reason? }`
/// for the web-settings subdomain toggle (web-content-hosting.md § Client
/// authoring UI). The reserved-label rule + the `<handle>.<domain>` URL live in
/// shared Rust (`fauna_core::web`), so the SPA never re-derives them and can't
/// drift from the nest's routing (priority #2). `disabled_reason` is the serde
/// enum string (`"NoHandle"` / `"ReservedLabel"` / `"NoServingDomain"`) or
/// `null`.
///
/// ⚠ `domain` **must** come from `webServingDomain` (the nest's own answer),
/// never from the address the SPA dialed and never from the sign-in reply's
/// `domain` — both compose hosts the nest will not answer on.
#[wasm_bindgen(js_name = webSubdomainView)]
pub fn web_subdomain_view(
    enabled: bool,
    handle: Option<String>,
    domain: String,
) -> Result<JsValue, JsValue> {
    let view = fauna_client_web::subdomain_view(enabled, handle.as_deref(), &domain);
    crate::rpc::to_js(&view)
}

/// `fauna_core::web::apex_url` → the `https://<domain>/` the apex serves at, for
/// the admin-web apex-picker info line. Shared with the linux lead + the native
/// FFI `webApexUrl` so the URL hint is one source of truth.
#[wasm_bindgen(js_name = webApexUrl)]
pub fn web_apex_url(domain: String) -> String {
    fauna_core::web::apex_url(&domain)
}

/// `fauna_client_web::site_link_view` → `{ origin?, disabled_reason? }` — where
/// the actor's published content is reachable, resolving **active custom domain
/// > enabled subdomain** (web-content-hosting.md § Published-post management).
/// `domains` is the `fauna.web.domain.get` rows as `[{ domain, status }, …]`
/// (the reply's own shape — pass it through); only `active` counts.
/// `disabled_reason` is the serde enum string (`"SubdomainDisabled"` /
/// `"NoHandle"` / `"ReservedLabel"` / `"NoServingDomain"`) or `null` — the
/// copy-link affordances disable with it rather than handing out a dead link.
///
/// ⚠ `domain` **must** come from `webServingDomain` — see [`web_subdomain_view`].
#[wasm_bindgen(js_name = webSiteLinkView)]
pub fn web_site_link_view(
    domains: JsValue,
    subdomain_enabled: bool,
    handle: Option<String>,
    domain: String,
) -> Result<JsValue, JsValue> {
    let domains: Vec<fauna_client_web::WebDomainRow> =
        if domains.is_null() || domains.is_undefined() {
            Vec::new()
        } else {
            crate::rpc::from_js(domains)?
        };
    let view =
        fauna_client_web::site_link_view(&domains, subdomain_enabled, handle.as_deref(), &domain);
    crate::rpc::to_js(&view)
}

/// `fauna_client_web::post_page_url` → the tokenless public page URL for a
/// published post: the *Copy web link* value (a visitor with no token gets the
/// teaser). Shared so no app re-derives `post/{slug}.html`.
#[wasm_bindgen(js_name = webPostPageUrl)]
pub fn web_post_page_url(origin: String, slug: String) -> String {
    fauna_client_web::post_page_url(&origin, &slug)
}

/// `fauna_client_web::tokened_url` → the full-access URL for a minted paywall
/// link: the *Copy paywall link* value. `path` must be the mint reply's own
/// `path`, never a client-rebuilt one.
#[wasm_bindgen(js_name = webTokenedUrl)]
pub fn web_tokened_url(origin: String, path: String, token: String) -> String {
    fauna_client_web::tokened_url(&origin, &path, &token)
}

/// `fauna_client_web::disabled_reason_text` → a `LocalizedText` `{ key, args }`
/// the SPA resolves through `L()` — the "your posts have no public address,
/// and here's why" line for a [`web_site_link_view`] result with no origin.
/// `reason` is the serde enum string (`"SubdomainDisabled"` / `"NoHandle"` /
/// `"ReservedLabel"` / `"NoServingDomain"`) or `null`/`undefined` — see
/// [`web_site_link_view`].
#[wasm_bindgen(js_name = webDisabledReasonText)]
pub fn web_disabled_reason_text(reason: JsValue) -> Result<JsValue, JsValue> {
    let reason: Option<fauna_client_web::SiteLinkDisabledReason> =
        if reason.is_null() || reason.is_undefined() {
            None
        } else {
            crate::rpc::from_js(reason)?
        };
    crate::rpc::to_js(&fauna_client_web::disabled_reason_text(reason))
}

#[wasm_bindgen]
pub fn get_recipients(payload: &[u8]) -> Result<String, JsValue> {
    get_recipients_inner(payload).map_err(|e| JsValue::from_str(&e))
}

#[wasm_bindgen]
pub fn build_signed_email(
    secret_hex: &str,
    to_hex: &str,
    subject: &str,
    body: &str,
    node_url: &str,
) -> Result<Vec<u8>, JsValue> {
    build_signed_email_inner(secret_hex, to_hex, subject, body, node_url)
        .map_err(|e| JsValue::from_str(&e))
}

/// `fauna_client_core::email::build_knock_payload` → the canonical signed
/// `(ContactRequest, Post)` tuple for an outbound **knock** (contact request),
/// ready for the `fauna.inbox.send` kind. The browser twin of the native
/// `build_knock_payload` UniFFI face (android/windows/linux build their knock
/// through it), so the `"Knock"` wire sentinel stays a Rust const and can never
/// drift into a per-app literal (priority #2). See
/// `docs/goal/architecture/api-layers.md` § Contacts & Knocks.
#[wasm_bindgen(js_name = buildKnockPayload)]
pub fn build_knock_payload(
    secret_hex: &str,
    to_hex: &str,
    node_url: &str,
) -> Result<Vec<u8>, JsValue> {
    build_knock_payload_inner(secret_hex, to_hex, node_url).map_err(|e| JsValue::from_str(&e))
}

// NOTE (2026-07-15 dark-rail audit): the wasm `build_signed_email_with_media`
// export was deleted with its FFI twin — no caller on any surface (the JS
// wrapper was removed in the MLS cutover; media DMs ride the MLS conversation
// attachment path).

#[wasm_bindgen]
pub fn decode_email(payload: &[u8]) -> Result<String, JsValue> {
    decode_email_inner(payload).map_err(|e| JsValue::from_str(&e))
}

#[wasm_bindgen]
pub fn build_register_request(
    secret_hex: &str,
    handle: &str,
    domain: &str,
) -> Result<String, JsValue> {
    build_register_request_inner(secret_hex, handle, domain).map_err(|e| JsValue::from_str(&e))
}

// ── Pre-identity auth-bootstrap + pairing helpers (wasm-only: they ride the
//    browser anonymous WS-RPC connection / the wasm `fauna-client-pair` seam) ──

/// The identity every login signature on `client` binds, read off the
/// connection and checked against `origin`'s TOFU pin **before** anything is
/// signed (`login.md` § Binding the nest; the web arm of the one shared
/// reader, `fauna_client_core::nest_trust::read_login_binding`). A browser
/// cannot compare the served cert's SPKI, so the read is possession-only and
/// the pin check on it is what catches a relaying origin from the second
/// contact on — the same residual the verify-reply pin already documents,
/// and strictly narrower than the unbound form's. Errors carry the SPA's
/// classification prefixes (`transient:` / `outdated:` / the distinctive
/// nest-identity-changed rejection).
#[cfg(target_arch = "wasm32")]
pub(crate) async fn bound_login_identity(
    client: &fauna_rpc_wasm::AnonymousWsRpcClient,
    origin: &str,
) -> Result<[u8; 32], JsValue> {
    use fauna_client_core::nest_trust::{LoginBindingError, read_login_binding};
    let nest_id = match read_login_binding(client, None).await {
        Ok(id) => id,
        Err(LoginBindingError::Refused(e)) | Err(LoginBindingError::Transport(e)) => {
            return Err(map_silent_err(e));
        }
        Err(e) => return Err(transient_js(&e.to_string())),
    };
    crate::nest_identity::check_pin_and_maybe_repin(client, origin, Some(nest_id)).await?;
    Ok(nest_id)
}

/// Build a signed `fauna.auth.handshake` request as the keypair owner — the
/// one-shot machine-to-machine mints (the backup-destination resolve in
/// `rpc.rs`, the successor's first mint in `succession.rs`). No app-held
/// bearer is minted this way: the SPA's re-mint rides `challengeVerify`
/// (`login.md` § When to use which). Reads and pin-checks the identity the
/// signature binds ([`bound_login_identity`]) and folds a fresh per-request
/// nonce into the signature for the nest's replay-guard uniqueness
/// (auth-handshake finding #1); `getrandom::fill` uses the `wasm_js` backend.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn build_handshake_request(
    client: &fauna_rpc_wasm::AnonymousWsRpcClient,
    origin: &str,
    kp: &ActorKeypair,
) -> Result<fauna_protocol::auth::HandshakeRequest, JsValue> {
    use ed25519_dalek::Signer;
    let nest_id = bound_login_identity(client, origin).await?;
    let actor_id = kp.actor_id().0;
    let timestamp = Timestamp::now_millis();
    let mut nonce = [0u8; 32];
    getrandom::fill(&mut nonce).expect("OS RNG available for handshake nonce");
    let msg =
        fauna_protocol::auth::handshake_signed_message(&actor_id, timestamp, &nest_id, &nonce);
    let signature = kp.signing_key().sign(&msg);
    Ok(fauna_protocol::auth::HandshakeRequest {
        actor_id: hex::encode(actor_id),
        timestamp,
        signature: hex::encode(signature.to_bytes()),
        client_nonce: fauna_protocol::ByteBuf::from(nonce.to_vec()),
        nest_id: hex::encode(nest_id),
        extra: Default::default(),
    })
}

/// Silent sign-in over the pre-identity (anonymous) WS-RPC challenge/verify
/// ceremony — the web twin of the deleted HTTP `POST /api/v1/auth/{challenge,
/// verify}`, and the SPA's **only** bearer mint: launch (`silentSignIn`) and
/// every re-mint (`getAuthToken`, the TTL and 4401 refresh) alike, because a
/// device whose clock is hours wrong must stay signed in on it (`login.md`
/// § When to use which — the handshake's ±30 s client timestamp is the wrong
/// freshness rule for a user's device). Origin-agnostic: the home nest *and*
/// a second nest (the both-ends pairing peer); the anonymous WS is CORS-exempt.
/// Opens a
/// one-shot anonymous connection to `origin`, requests a fresh challenge nonce
/// for the actor (`fauna.auth.challenge`), signs the **domain-tagged,
/// nest-bound** `AUTH_VERIFY_V2 ‖ actor_id ‖ nonce ‖ nest_id` with the shared
/// core ceremony (`fauna_client_core::auth::build_challenge_verify`, no
/// timestamp — the nonce supplies freshness; the nest identity read and
/// pin-checked first, [`bound_login_identity`]), and verifies it
/// (`fauna.auth.verify`).
///
/// Resolves a JSON string `{ token, token_id, handle, domain, tier, expires_at,
/// expires_in }` on success — `expires_in` is the lifetime in seconds from the
/// reply, which the SPA anchors on its
/// own clock at receipt (`login.md` § Token lifetime on the client's clock);
/// `expires_at` is the nest's clock, not the anchor — or JS `null` when
/// the actor isn't registered on `origin`
/// (`fauna.auth.not_registered` — the launch flow's drop-into-onboarding signal,
/// mirroring the HTTP twin's 404 → null). A transport failure rejects with a
/// `"transient: …"`-prefixed string so the onboarding launch screen surfaces its
/// Retry CTA (mirrors the HTTP path's `TypeError` → transient); a degraded nest
/// (`fauna.nest.outdated`) rejects with an `"outdated: …"`-prefixed localized
/// message so the screen routes to a non-retry "update your nest" surface
/// (version-compatibility.md Dim 4); any other server RPC error or codec failure
/// rejects with the raw message (→ unreachable).
///
/// No TLS channel binding: the browser owns TLS verification
/// (`docs/goal/architecture/security.md` § Axis 1, native-only). The nest's
/// identity is nonetheless possession-verified and checked against the TOFU
/// pin **before** the signature ([`bound_login_identity`]), so every re-mint
/// is also a post-auth identity re-check (§ Post-auth surfacing, channel 1).
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = challengeVerify)]
pub fn challenge_verify(secret_hex: String, origin: String) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        challenge_verify_inner(&secret_hex, &origin).await
    })
}

#[cfg(target_arch = "wasm32")]
async fn challenge_verify_inner(secret_hex: &str, origin: &str) -> Result<JsValue, JsValue> {
    use fauna_protocol::RpcRequester;
    use fauna_protocol::auth::{ChallengeReply, ChallengeRequest, VerifyReply, VerifyRequest};
    use fauna_rpc_wasm::WsRpcError;

    let kp = keypair_from_hex(secret_hex).map_err(|e| JsValue::from_str(&e))?;
    let actor_id_hex = hex::encode(kp.actor_id().0);

    // One anonymous connection carries both legs: the server keys the nonce by
    // actor, not by connection, so challenge then verify on the same socket
    // matches the HTTP twin's two stateless calls.
    let client = fauna_rpc_wasm::AnonymousWsRpcClient::connect(origin)
        .map_err(|e| transient_js(&format!("anonymous connect {origin}: {e}")))?;

    // 0. The identity this login binds, checked against the origin's pin
    //    before anything is signed (`login.md` § Binding the nest).
    let nest_id = bound_login_identity(&client, origin).await?;

    // 1. Request a fresh challenge nonce.
    let chal: ChallengeReply = client
        .request(
            "fauna.auth.challenge",
            ChallengeRequest {
                actor_id: actor_id_hex,
                extra: Default::default(),
            },
        )
        .await
        .map_err(map_silent_err)?;

    // 2. Sign the tagged, nest-bound `AUTH_VERIFY_V2 ‖ actor_id ‖ nonce ‖
    //    nest_id` (shared core ceremony).
    let nonce_bytes = hex::decode(&chal.nonce)
        .map_err(|e| JsValue::from_str(&format!("invalid nonce hex: {e}")))?;
    let nonce: [u8; 32] = nonce_bytes
        .try_into()
        .map_err(|_| JsValue::from_str("nonce must be 32 bytes"))?;
    let signed = fauna_client_core::auth::build_challenge_verify(&kp, &nonce, &nest_id);
    // No NT-1 client nonce: the verify reply's channel-binding proof is not
    // consulted on web any more — the identity was proven (over a fresh
    // client nonce) and pin-checked by the opening read above, and the login
    // signature is what holds the nest to it (`login.md` § Binding the nest).
    let verify_req = VerifyRequest {
        actor_id: hex::encode(signed.actor_id),
        nonce: hex::encode(signed.nonce),
        signature: hex::encode(signed.signature),
        client_nonce: None,
        nest_id: hex::encode(signed.nest_id),
        extra: Default::default(),
    };

    // 3. Verify → bearer + cached identity, or `null` when unregistered.
    let reply: VerifyReply = match client.request("fauna.auth.verify", verify_req).await {
        Ok(reply) => reply,
        Err(WsRpcError::Rpc(ref e)) if e.code == "fauna.auth.not_registered" => {
            return Ok(JsValue::NULL);
        }
        Err(e) => return Err(map_silent_err(e)),
    };

    // The nest-identity verdict (the TOFU pin, security.md § Transport trust)
    // ran BEFORE the signature, on the identity the opening read proved
    // (`bound_login_identity`); nothing on the reply is re-checked here.

    Ok(JsValue::from_str(
        &serde_json::json!({
            "token": reply.token,
            "token_id": reply.token_id,
            "handle": reply.handle,
            "domain": reply.domain,
            "tier": reply.tier,
            "expires_at": reply.expires_at,
            "expires_in": reply.expires_in,
        })
        .to_string(),
    ))
}

/// Map an anonymous-WS transport error to a `JsValue` the SPA can classify by
/// prefix: connection/timeout failures get `"transient: …"` (→ Retry CTA); a
/// degraded nest's `fauna.nest.outdated` gets `"outdated: …"` carrying the
/// localized actionable banner (→ a non-retry "update your nest" surface,
/// version-compatibility.md Dim 4); everything else (a server RPC error other
/// than the `not_registered` handled at the call site, or a codec failure)
/// surfaces as-is (→ unreachable).
#[cfg(target_arch = "wasm32")]
fn map_silent_err(e: fauna_rpc_wasm::WsRpcError) -> JsValue {
    use fauna_protocol::RpcErrorAction;
    use fauna_rpc_wasm::WsRpcError::*;
    // A degraded nest answers `fauna.nest.outdated` to either ceremony
    // round-trip. Route it to its own non-retry signal — distinguishable from
    // the retryable `transient:` faults and the generic "unreachable" bucket —
    // so the SPA prompts an update instead of spinning a retry loop. Keyed on
    // the shared `RpcError::action()` classifier (the same join the launch
    // machine + FFI use), so the wire-code match lives in one place.
    if let Rpc(rpc) = &e
        && rpc.action() == RpcErrorAction::NeedsUpdate
    {
        return outdated_js(rpc.localized());
    }
    // The identity was SUCCEEDED — account-level and permanent, not transport.
    // Without this arm it fell into the opaque bucket below, where the SPA's
    // background identity refresh logs a warning and swallows it: a succeeded
    // device kept running against a nest that had already refused it, with the
    // user told nothing. This is web's analogue of the four FFI apps'
    // `FfiError::IdentitySuperseded` (`libs/fauna-ffi/src/auth.rs`), and the
    // gap `identity-succession.md` § Implementation status today named.
    //
    // The successor rides the prefix because the SPA has no other channel for
    // it on this path — but it is the nest's CLAIM, never presented as fact:
    // the surface it reaches shows the claim-free wording and only names a
    // successor the registration chain independently proves. Keyed on
    // `superseded_by()` rather than the code alone, exactly as the launch
    // machine keys it: a refusal naming no successor keeps the old mapping
    // instead of inventing an empty one.
    if let Rpc(rpc) = &e
        && let Some(successor) = rpc.superseded_by()
    {
        return superseded_js(&hex::encode(successor));
    }
    // The account is LOCKED OUT (`fauna.auth.account_locked`, `login.md`
    // § Silent Challenge): terminal until `locked_until`, so it gets its own
    // prefix rather than the opaque bucket (a retry loop re-earns the refusal).
    // Keyed on `locked_until_secs()` rather than the code alone, as the launch
    // machine's mapping is: a refusal with no readable time keeps the old
    // mapping instead of inventing one.
    if let Rpc(rpc) = &e
        && let Some(locked_until) = rpc.locked_until_secs()
    {
        return locked_js(locked_until);
    }
    let transient = matches!(e, Connect(_) | Disconnected | NotConnected | Timeout);
    let msg = e.to_string();
    if transient {
        transient_js(&msg)
    } else {
        JsValue::from_str(&msg)
    }
}

#[cfg(target_arch = "wasm32")]
fn transient_js(detail: &str) -> JsValue {
    JsValue::from_str(&format!("transient: {detail}"))
}

/// A `fauna.auth.superseded` refusal: this identity was succeeded and the
/// account belongs to a different keypair now. `detail` is the CLAIMED
/// successor's 64-hex actor id — carried so the escalation can hand it on, not
/// so it can be shown; the nest is enforcer and distributor, never authorizer
/// (`identity-succession.md` § Propagation → *Own device fleet*).
#[cfg(target_arch = "wasm32")]
fn superseded_js(detail: &str) -> JsValue {
    JsValue::from_str(&format!("superseded: {detail}"))
}

/// A `fauna.auth.account_locked` refusal: `detail` is `locked_until` in Unix
/// seconds, which the SPA parses off the `locked:` prefix (`auth-errors.ts`).
#[cfg(target_arch = "wasm32")]
fn locked_js(locked_until_secs: u64) -> JsValue {
    JsValue::from_str(&format!("locked: {locked_until_secs}"))
}

/// A degraded-nest (`fauna.nest.outdated`) rejection the SPA routes to a
/// non-retry "update your nest" surface — the web sibling of the FFI's
/// `FfiError::NestOutdated` (version-compatibility.md Dim 4). `detail` is the
/// already-localized actionable message.
#[cfg(target_arch = "wasm32")]
fn outdated_js(detail: &str) -> JsValue {
    JsValue::from_str(&format!("outdated: {detail}"))
}

// ── Pre-identity public discovery ───────────────────────────────────────────
//
// The anonymous-WS replacements for the deleted HTTP discovery twins
// `GET /api/v1/{node-info,resolve-node,actor/by-handle}` (`api-layers.md` §
// Public). Each rides the same one-shot `AnonymousWsRpcClient` as
// `challengeVerify` — the four discovery kinds are allowlisted on
// the pre-identity surface (`bins/fauna-nest/src/pre_identity_allowlist.rs`) and
// resolve the *same* `discovery_core` the HTTP twins did, so behavior is
// identical. No keypair: discovery is unsigned. The replies are emitted as JSON
// strings shaped for the existing `$lib/{api,resolve}.ts` consumers (no
// consumer-side churn). `fauna.handle.available` has no web caller (the wasm
// `OnboardingMachine` owns handle validation), so it gets no face here.

/// `fauna.nest.info` — the nest's public metadata (domain, version, registration
/// policy). Resolves the JSON `{ domain, version, registration }` the SPA's
/// `NestInfoResponse` expects (`registration` is `null` on a domain-less nest).
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = nestInfo)]
pub fn nest_info(origin: String) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move { nest_info_inner(&origin).await })
}

#[cfg(target_arch = "wasm32")]
async fn nest_info_inner(origin: &str) -> Result<JsValue, JsValue> {
    use fauna_protocol::RpcRequester;
    use fauna_protocol::discovery::{NestInfoReply, NestInfoRequest};

    let client = fauna_rpc_wasm::AnonymousWsRpcClient::connect(origin)
        .map_err(|e| transient_js(&format!("anonymous connect {origin}: {e}")))?;
    let reply: NestInfoReply = client
        .request("fauna.nest.info", NestInfoRequest::default())
        .await
        .map_err(map_silent_err)?;
    let registration = reply.registration.map(|r| {
        serde_json::json!({
            "tiers": r.tiers,
            "handle_domain": r.handle_domain.unwrap_or_default(),
        })
    });
    Ok(JsValue::from_str(
        &serde_json::json!({
            "domain": reply.domain,
            "version": reply.version,
            "registration": registration,
        })
        .to_string(),
    ))
}

/// `fauna.nest.resolve` — resolve a domain to its canonical fauna node URL (via
/// SRV). Resolves the bare URL string (the HTTP twin's `{"url": …}` body).
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = nestResolve)]
pub fn nest_resolve(domain: String, origin: String) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(
        async move { nest_resolve_inner(&domain, &origin).await },
    )
}

#[cfg(target_arch = "wasm32")]
async fn nest_resolve_inner(domain: &str, origin: &str) -> Result<JsValue, JsValue> {
    use fauna_protocol::RpcRequester;
    use fauna_protocol::discovery::{NestResolveReply, NestResolveRequest};

    let client = fauna_rpc_wasm::AnonymousWsRpcClient::connect(origin)
        .map_err(|e| transient_js(&format!("anonymous connect {origin}: {e}")))?;
    let reply: NestResolveReply = client
        .request(
            "fauna.nest.resolve",
            NestResolveRequest {
                domain: domain.to_string(),
                extra: Default::default(),
            },
        )
        .await
        .map_err(map_silent_err)?;
    Ok(JsValue::from_str(&reply.url))
}

/// `fauna.actor.by_handle` — resolve a human-readable handle to an actor ID for
/// addressing. Resolves the JSON `{ actor_id, handle, domain }` the SPA's
/// `resolveHandle` consumer expects. Origin-agnostic: the anonymous WS is
/// CORS-exempt, so a *cross-origin* (remote-nest) handle resolve works where the
/// HTTP `/actor/by-handle` fetch was CORS-blocked.
///
/// `domain` is the optional typed `@domain` qualifier. Given, the resolved
/// `handle`/`domain` are the TYPED pair — the dial names the peer and the reply's
/// echo is never read for identity (`foreign-handle-resolution.md` § Peer-auth
/// model; rule shared with the UniFFI face and linux in
/// `fauna_client_core::find_user`) — and it travels as the multi-domain
/// qualifier, so a nest that does not serve it rejects with
/// `fauna.actor.domain_not_local` (`mail-multidomain.md` § Multi-domain handles
/// § Resolution). `undefined` (a bare handle, resolved on the home nest) reports
/// the identity domain.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = actorByHandle)]
pub fn actor_by_handle(handle: String, origin: String, domain: Option<String>) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        actor_by_handle_inner(&handle, &origin, domain).await
    })
}

#[cfg(target_arch = "wasm32")]
async fn actor_by_handle_inner(
    handle: &str,
    origin: &str,
    domain: Option<String>,
) -> Result<JsValue, JsValue> {
    let client = fauna_rpc_wasm::AnonymousWsRpcClient::connect(origin)
        .map_err(|e| transient_js(&format!("anonymous connect {origin}: {e}")))?;
    let found =
        fauna_client_core::find_user::find_user_by_handle(&client, handle, domain.as_deref())
            .await
            .map_err(map_silent_err)?;
    Ok(JsValue::from_str(
        &serde_json::json!({
            "actor_id": found.actor_id,
            "handle": found.handle,
            "domain": found.domain,
        })
        .to_string(),
    ))
}

// ── Launch-path succession verification ─────────────────────────────────────

/// The **verified** successor of a refused identity — the wasm face of
/// `fauna_client_recovery::resolve_successor`. Resolves the successor's 64-hex
/// actor id, or `null` when the registration chain authorizes none.
///
/// **Why anonymous, and why it can be.** The refused identity cannot
/// authenticate — that is exactly what the `fauna.auth.superseded` refusal
/// means — so this walk would be impossible over a session connection. It works
/// because `succession.lookup` and `registration.chain` are **pre-identity**
/// kinds, like the discovery calls above.
///
/// **Why the successor the refusal NAMED is not a parameter.** The walk returns
/// what the chain *authorizes*; the nest's claim is an untrusted hint that plays
/// no part in the verdict — the nest is enforcer and distributor, never
/// authorizer (`identity-succession.md` § Propagation → *Own device fleet*). The
/// native twin, `fauna-tui`'s `verify_superseded_successor`, passes `None` for
/// the same reason, and its doc carries the full argument.
///
/// **Why this crate rather than `fauna-wasm-launch`, where the caller lives.**
/// The `AnonymousWsRpcClient` → `RecoveryClient` → `fauna_client_recovery`
/// plumbing is already here (`src/succession.rs` builds it verbatim), whereas
/// the launch chunk's wasm32-scoped dependency table exists precisely to keep
/// that chunk small — it loads on every page. The onboarding page already loads
/// this chunk (`$lib/accounts` → `ensureWasm`), so the caller pays nothing new.
///
/// Rejects **only** on an unreadable actor id. Every expected failure —
/// unreachable nest, empty lookup, a non-contiguous or hostile chain — resolves
/// `null`, because the correct fallback is the claim-free message the caller is
/// already showing: telling the user less is always safe, and naming an
/// unverified successor is the nest-as-authorizer trust the goal doc forbids.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = resolveVerifiedSuccessor)]
pub fn resolve_verified_successor(origin: String, old_actor_id_hex: String) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        resolve_verified_successor_inner(&origin, &old_actor_id_hex).await
    })
}

#[cfg(target_arch = "wasm32")]
async fn resolve_verified_successor_inner(
    origin: &str,
    old_actor_id_hex: &str,
) -> Result<JsValue, JsValue> {
    let old_actor_id = fauna_core::identity::ActorId::from_hex(old_actor_id_hex)
        .map_err(|e| JsValue::from_str(&format!("unreadable actor id: {e}")))?;
    let Ok(anon) = fauna_rpc_wasm::AnonymousWsRpcClient::connect(origin) else {
        // Unreachable nest — the claim-free message stands.
        return Ok(JsValue::NULL);
    };
    let client = fauna_client_recovery::RecoveryClient::new(anon);
    match fauna_client_recovery::resolve_successor(&client, old_actor_id, None).await {
        Ok(Some(verified)) => Ok(JsValue::from_str(&verified.new_actor_id.to_hex())),
        Ok(None) => {
            // The nest refused as superseded, yet serves no succession for this
            // identity. Nothing to show the user — but precisely the
            // disagreement an admin wants in the log.
            tracing::warn!("[launch] refused as superseded, yet the chain shows no succession");
            Ok(JsValue::NULL)
        }
        Err(e) => {
            tracing::warn!("[launch] could not verify the succession: {e}");
            Ok(JsValue::NULL)
        }
    }
}

// ── The succession's closing act: the successor's owed kit ──────────────────
//
// The three faces of the obligation an identity succession hands across its own
// account switch (`identity-succession.md` § The RecoveryKey → *At succession*).
// The slot itself and the full argument live in `src/succession.rs`, beside the
// three siblings the same ceremony parks there.
//
// **Free functions, not `WsRpcClient` methods** — unlike the sibling reads
// `ephemeralReviewPassActive` / `successionSweepCopy`, which are also
// synchronous local reads. Those are asked from the Settings page, where a
// connected client is already in hand; `successionKitOwed` is asked at BOOT to
// decide where this launch belongs, and routing that through `getClient` would
// make the navigation wait on a WebSocket for an answer that never leaves the
// tab. Keyed by the actor id directly for the same reason.

/// Whether this identity owes itself a fresh RecoveryKey from a succession it
/// just ran — a **peek**, for the launch's navigation decision only. See
/// `succession::succession_kit_owed` for why claiming here would be wrong.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = successionKitOwed)]
pub fn succession_kit_owed(actor_id_hex: String) -> bool {
    crate::succession::succession_kit_owed(&actor_id_hex)
}

/// Take the owed-kit obligation, clearing it in the same breath — the one-shot
/// claim, made by the section that renders the kit. `false` on every ordinary
/// sign-in.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = claimOwedSuccessionKit)]
pub fn claim_owed_succession_kit(actor_id_hex: String) -> bool {
    crate::succession::claim_owed_succession_kit(&actor_id_hex)
}

/// Put back an obligation whose mint never reached the screen. ⚠ A failed mint
/// must re-arm, never spend — the successor's first mint races its own
/// reconnect on every platform. Full argument at
/// `succession::rearm_owed_succession_kit`.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = rearmOwedSuccessionKit)]
pub fn rearm_owed_succession_kit(actor_id_hex: String) {
    crate::succession::rearm_owed_succession_kit(&actor_id_hex)
}

/// Classify a user-entered link-form value (shared `fauna_client_pair::
/// classify_link_input`) into the action the `linked-nests` form dispatches — a
/// 64-hex identity → `{"NestId":{"nest_id":…}}` (single-end `Link`), any other
/// value → `{"NestUrl":{"nest_url":…}}` (both-ends `LinkBoth`). The shell
/// classifies identically on every app (priority #2/#3); linux does it in
/// Rust, web over this export.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = classifyLinkInput)]
pub fn classify_link_input(raw: &str) -> Result<String, JsValue> {
    let input = fauna_client_pair::classify_link_input(raw);
    serde_json::to_string(&input).map_err(crate::rpc::err_to_js)
}

#[wasm_bindgen]
pub fn build_invite_request_submit(
    secret_hex: &str,
    handle: &str,
    message: &str,
) -> Result<String, JsValue> {
    build_invite_request_submit_inner(secret_hex, handle, message)
        .map_err(|e| JsValue::from_str(&e))
}

#[wasm_bindgen]
pub fn build_invite_request_cancel(secret_hex: &str) -> Result<String, JsValue> {
    build_invite_request_cancel_inner(secret_hex).map_err(|e| JsValue::from_str(&e))
}

#[wasm_bindgen]
pub fn chunk_file(data: &[u8]) -> Result<String, JsValue> {
    chunk_file_inner(data).map_err(|e| JsValue::from_str(&e))
}

#[wasm_bindgen]
pub fn extract_chunks(data: &[u8], manifest_json: &str) -> Result<String, JsValue> {
    extract_chunks_inner(data, manifest_json).map_err(|e| JsValue::from_str(&e))
}

#[wasm_bindgen]
pub fn reassemble_chunks(chunks_json: &str) -> Result<Vec<u8>, JsValue> {
    reassemble_chunks_inner(chunks_json).map_err(|e| JsValue::from_str(&e))
}

// NOTE (2026-07-15 dark-rail audit): the speculative `wasm_content_hash` and
// `wasm_generate_device_id` exports were deleted — web hashing goes through
// `chunk_file` (per-chunk BLAKE3 in the manifest) and the web device id is a
// deliberate JS CSPRNG (`push.ts::getDeviceId`); neither export ever had a
// caller.

#[wasm_bindgen]
pub fn build_post(
    secret_hex: &str,
    body: &str,
    tags_json: &str,
    reply_to_hex: &str,
) -> Result<Vec<u8>, JsValue> {
    build_post_inner(secret_hex, body, tags_json, reply_to_hex).map_err(|e| JsValue::from_str(&e))
}

#[wasm_bindgen]
pub fn build_post_with_media(
    secret_hex: &str,
    body: &str,
    tags_json: &str,
    media_items_json: &str,
    reply_to_hex: &str,
) -> Result<Vec<u8>, JsValue> {
    build_post_with_media_inner(secret_hex, body, tags_json, media_items_json, reply_to_hex)
        .map_err(|e| JsValue::from_str(&e))
}

#[wasm_bindgen]
pub fn decode_post(data: &[u8]) -> Result<String, JsValue> {
    decode_post_inner(data).map_err(|e| JsValue::from_str(&e))
}

/// `fauna_core::data::Post::decode_resolved_bytes(data).map(Post::body_text)` — the
/// resolved-post plain-text extraction the tier-1 spam-model client-write path trains
/// on (`train-correction-button` over a server queue row): the raw bytes `fauna.posts.get`
/// returns, decoded via the SAME path the nest indexer/train handler uses (embed-as-bytes
/// signed post, falling back to a bare canonical `Post`), so a client-path train is
/// byte-identical with a nest-path train on the same post. `null` if the bytes decode as
/// neither shape. See `docs/goal/behavior/mail-spam.md` § Encrypted-mode interaction.
#[wasm_bindgen(js_name = postBodyText)]
pub fn post_body_text(data: &[u8]) -> Option<String> {
    Post::decode_resolved_bytes(data).map(|p| p.body_text())
}

// ── Core logic (testable on native) ──────────────────────────

/// Generate a new Ed25519 keypair. Returns the 64-char hex-encoded secret key.
pub fn generate_keypair_inner() -> String {
    let kp = fauna_client_core::identity::generate_keypair();
    hex::encode(kp.signing_key().to_bytes())
}

/// Derive the public ActorId (hex) from a 64-char hex secret key.
pub fn actor_id_from_secret_inner(secret_hex: &str) -> Result<String, String> {
    let kp = keypair_from_hex(secret_hex)?;
    Ok(hex::encode(fauna_client_core::identity::actor_id(&kp)))
}

/// Extract the "to" field from a canonical dag-cbor-encoded email payload.
/// Returns a JSON array of hex-encoded ActorIds.
pub fn get_recipients_inner(payload: &[u8]) -> Result<String, String> {
    let recipients = fauna_client_core::email::get_recipients(payload).map_err(|e| e.0)?;
    let hex_list: Vec<String> = recipients.iter().map(hex::encode).collect();
    serde_json::to_string(&hex_list).map_err(|e| format!("json serialize: {e}"))
}

// ── Email builders/decoder (delegates to fauna-client-core) ──

pub fn build_signed_email_inner(
    secret_hex: &str,
    to_hex: &str,
    subject: &str,
    body: &str,
    node_url: &str,
) -> Result<Vec<u8>, String> {
    let kp = keypair_from_hex(secret_hex)?;
    let to_bytes = hex::decode(to_hex).map_err(|e| format!("bad to hex: {e}"))?;
    let to: [u8; 32] = to_bytes
        .try_into()
        .map_err(|_| "to must be 64 hex chars".to_string())?;
    fauna_client_core::email::build_signed_email(&kp, &to, subject, body, node_url).map_err(|e| e.0)
}

pub fn build_knock_payload_inner(
    secret_hex: &str,
    to_hex: &str,
    node_url: &str,
) -> Result<Vec<u8>, String> {
    let kp = keypair_from_hex(secret_hex)?;
    let to_bytes = hex::decode(to_hex).map_err(|e| format!("bad to hex: {e}"))?;
    let to: [u8; 32] = to_bytes
        .try_into()
        .map_err(|_| "to must be 64 hex chars".to_string())?;
    fauna_client_core::email::build_knock_payload(&kp, &to, node_url).map_err(|e| e.0)
}

pub fn decode_email_inner(payload: &[u8]) -> Result<String, String> {
    fauna_client_core::email::decode_email_json(payload).map_err(|e| e.0)
}

/// Build a signed registration request body (JSON string).
///
/// Signs the domain-tagged, length-prefixed
/// `fauna_protocol::account::register_signed_message` — matching the nest's
/// `fauna.account.register` validation.
pub fn build_register_request_inner(
    secret_hex: &str,
    handle: &str,
    domain: &str,
) -> Result<String, String> {
    let kp = keypair_from_hex(secret_hex)?;
    let req = fauna_client_core::auth::build_register_request(&kp, handle, domain);
    let json = serde_json::json!({
        "actor_id": hex::encode(req.actor_id),
        "handle": req.handle,
        "timestamp": req.timestamp,
        "signature": hex::encode(req.signature),
    });
    Ok(json.to_string())
}

pub fn build_invite_request_submit_inner(
    secret_hex: &str,
    handle: &str,
    message: &str,
) -> Result<String, String> {
    let kp = keypair_from_hex(secret_hex)?;
    let req = fauna_client_core::auth::build_invite_request_submit(&kp, handle, message);
    let json = serde_json::json!({
        "actor_id": hex::encode(req.actor_id),
        "handle": req.handle,
        "message": req.message,
        "timestamp": req.timestamp,
        "signature": hex::encode(req.signature),
    });
    Ok(json.to_string())
}

pub fn build_invite_request_cancel_inner(secret_hex: &str) -> Result<String, String> {
    let kp = keypair_from_hex(secret_hex)?;
    let req = fauna_client_core::auth::build_invite_request_cancel(&kp);
    let json = serde_json::json!({
        "actor_id": hex::encode(req.actor_id),
        "timestamp": req.timestamp,
        "signature": hex::encode(req.signature),
    });
    Ok(json.to_string())
}

// ── Post builders ────────────────────────────────────────────

/// Build and sign a bare Post (not wrapped in ContactRequest).
/// Returns dag-cbor-encoded Post bytes.
pub fn build_post_inner(
    secret_hex: &str,
    body: &str,
    tags_json: &str,
    reply_to_hex: &str,
) -> Result<Vec<u8>, String> {
    let kp = keypair_from_hex(secret_hex)?;
    let tags: Vec<String> =
        serde_json::from_str(tags_json).map_err(|e| format!("bad tags JSON: {e}"))?;
    let reply_to = parse_reply_to_hex(reply_to_hex)?;
    fauna_client_core::post::build_post(&kp, body, &tags, reply_to).map_err(|e| e.0)
}

pub fn build_post_with_media_inner(
    secret_hex: &str,
    body: &str,
    tags_json: &str,
    media_items_json: &str,
    reply_to_hex: &str,
) -> Result<Vec<u8>, String> {
    let kp = keypair_from_hex(secret_hex)?;
    let tags: Vec<String> =
        serde_json::from_str(tags_json).map_err(|e| format!("bad tags JSON: {e}"))?;
    let media_inputs: Vec<MediaItemInput> =
        serde_json::from_str(media_items_json).map_err(|e| format!("bad media JSON: {e}"))?;
    let items: Vec<MediaItem> = media_inputs
        .into_iter()
        .map(|mi| {
            let hash_bytes = hex::decode(&mi.hash).map_err(|e| format!("bad hash hex: {e}"))?;
            let hash_arr: [u8; 32] = hash_bytes
                .try_into()
                .map_err(|_| "hash must be 64 hex chars".to_string())?;
            Ok(MediaItem {
                blob_hash: ContentHash::from_digest_raw(hash_arr),
                media_type: mi.media_type,
                size_bytes: mi.size_bytes,
                dimensions: None,
                thumbnail: None,
                remote_url: None,
                alt: None,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let reply_to = parse_reply_to_hex(reply_to_hex)?;
    fauna_client_core::post::build_post_with_media(&kp, body, items, &tags, reply_to)
        .map_err(|e| e.0)
}

/// Decode a dag-cbor-encoded Post and return JSON with author, body, created_at, references, tags.
/// Mirrors the UniFFI `decode_post_full` in `libs/fauna-ffi/src/post.rs` — both
/// marshal `fauna_client_core::post::decode_post_shaped`'s platform-neutral
/// shape into their own binding's output. This face's JSON has never included
/// `facets`/`content_warning`; that stays true here (the shaped struct carries
/// them, this wrapper just doesn't serialize them).
pub fn decode_post_inner(data: &[u8]) -> Result<String, String> {
    let shaped = fauna_client_core::post::decode_post_shaped(data).map_err(|e| e.0)?;

    let references: Vec<serde_json::Value> = shaped
        .references
        .iter()
        .map(|r| {
            let mut obj = serde_json::json!({
                "type": r.ref_type,
                "post_id": r.post_id,
            });
            if let Some(emoji) = &r.emoji {
                obj["emoji"] = serde_json::Value::String(emoji.clone());
            }
            obj
        })
        .collect();

    let items: Vec<serde_json::Value> = shaped
        .items
        .iter()
        .map(|item| {
            serde_json::json!({
                "blob_hash": item.blob_hash,
                "media_type": item.media_type,
                "size_bytes": item.size_bytes,
            })
        })
        .collect();

    let result = serde_json::json!({
        "post_id": shaped.post_id,
        "author": shaped.author,
        "body": shaped.body,
        "created_at": shaped.created_at,
        "tags": shaped.tags,
        "references": references,
        "valid": shaped.valid,
        "authoring_origin": shaped.authoring_origin,
        "items": items,
    });

    serde_json::to_string(&result).map_err(|e| format!("json serialize: {e}"))
}

fn parse_reply_to_hex(reply_to_hex: &str) -> Result<Option<[u8; 36]>, String> {
    if reply_to_hex.is_empty() {
        return Ok(None);
    }
    let bytes = hex::decode(reply_to_hex).map_err(|e| format!("bad reply_to hex: {e}"))?;
    let arr: [u8; 36] = bytes
        .try_into()
        .map_err(|_| "reply_to must be 72 hex chars (36-byte CID)".to_string())?;
    Ok(Some(arr))
}

// ── Chunker ──────────────────────────────────────────────────

fn chunk_file_inner(data: &[u8]) -> Result<String, String> {
    let manifest = fauna_client_core::chunking::chunk_data(data);
    let chunks = fauna_client_core::chunking::extract_chunks(data, &manifest);

    let manifest_json = serde_json::json!({
        "file_hash": hex::encode(manifest.file_hash.digest()),
        "total_size": manifest.total_size,
        "chunk_count": manifest.chunk_hashes.len(),
    });

    let chunks_json: Vec<serde_json::Value> = chunks
        .into_iter()
        .map(|(hash, data)| {
            serde_json::json!({
                "hash": hex::encode(hash.digest()),
                "data": hex::encode(data),
            })
        })
        .collect();

    Ok(serde_json::json!({
        "manifest": manifest_json,
        "chunks": chunks_json,
    })
    .to_string())
}

fn extract_chunks_inner(data: &[u8], manifest_json: &str) -> Result<String, String> {
    use fauna_core::chunk::ChunkManifest;
    let manifest: ChunkManifest =
        serde_json::from_str(manifest_json).map_err(|e| format!("bad manifest JSON: {e}"))?;
    let chunks = fauna_client_core::chunking::extract_chunks(data, &manifest);
    let result: Vec<serde_json::Value> = chunks
        .into_iter()
        .map(|(hash, bytes)| {
            serde_json::json!({
                "hash": hex::encode(hash.digest()),
                "data": hex::encode(bytes),
            })
        })
        .collect();
    serde_json::to_string(&result).map_err(|e| format!("serialize error: {e}"))
}

fn reassemble_chunks_inner(chunks_json: &str) -> Result<Vec<u8>, String> {
    let chunks: Vec<String> =
        serde_json::from_str(chunks_json).map_err(|e| format!("bad chunks JSON: {e}"))?;
    let chunk_bytes: Result<Vec<Vec<u8>>, String> = chunks
        .iter()
        .map(|h| hex::decode(h).map_err(|e| format!("bad hex: {e}")))
        .collect();
    Ok(fauna_client_core::chunking::reassemble_chunks(
        &chunk_bytes?,
    ))
}

fn keypair_from_hex(hex_str: &str) -> Result<ActorKeypair, String> {
    let bytes = hex::decode(hex_str.trim()).map_err(|e| format!("bad hex: {e}"))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "secret must be 64 hex chars".to_string())?;
    Ok(ActorKeypair::from_secret(arr))
}

#[derive(serde::Deserialize)]
struct MediaItemInput {
    hash: String,
    media_type: String,
    size_bytes: u64,
}

#[cfg(test)]
mod upload_sidecar_tests {
    use super::{process_and_seal_library_inner, process_and_seal_public_post_inner};
    use fauna_media::audience::AudienceClass;
    use fauna_media::sidecar::UploadSidecar;

    fn decode_sidecar(bytes: &[u8]) -> UploadSidecar {
        UploadSidecar::from_dag_cbor(bytes).expect("sidecar decodes as UploadSidecar")
    }

    #[test]
    fn library_seals_under_backup_key_with_octet_stream_sidecar() {
        // Deterministic 32-byte identity seed (64 hex chars).
        let secret = "11".repeat(32);
        let raw = b"my private library photo bytes";
        let payload = process_and_seal_library_inner(raw, &secret).unwrap();

        let sidecar = decode_sidecar(&payload.sidecar());
        assert_eq!(sidecar.class, AudienceClass::Library);
        // The Library seal is opaque on the wire — the per-class verifier
        // requires octet-stream + no-C2PA for sealed classes.
        assert_eq!(sidecar.mime, "application/octet-stream");
        assert!(!sidecar.has_c2pa);
        // No browser thumbnail on web (stub `process_media`).
        assert!(sidecar.thumbnail_hash.is_none());

        // Library bytes are AEAD-sealed, not the plaintext: they differ and
        // carry at least the 12-byte nonce + 16-byte tag floor.
        assert_ne!(payload.bytes(), raw.to_vec());
        assert!(payload.bytes().len() >= raw.len() + 28);
    }

    #[test]
    fn library_rejects_a_malformed_secret() {
        assert!(process_and_seal_library_inner(b"x", "not-hex").is_err());
        assert!(process_and_seal_library_inner(b"x", "abcd").is_err()); // too short
    }

    #[test]
    fn public_post_passes_bytes_through_and_declares_browser_mime() {
        let raw = b"\x89PNG\r\n\x1a\n fake png bytes";
        let payload = process_and_seal_public_post_inner(raw, "image/png", false);

        let sidecar = decode_sidecar(&payload.sidecar());
        assert_eq!(sidecar.class, AudienceClass::PublicPost);
        // PublicPost bytes are plaintext; the uploader-declared MIME rides in
        // the sidecar (the stub `process_media` would otherwise report
        // octet-stream — the wrapper overrides it with the browser MIME so the
        // nest serves the right Content-Type).
        assert_eq!(sidecar.mime, "image/png");
        assert_eq!(payload.bytes(), raw.to_vec());
        assert_eq!(payload.mime(), "image/png");
    }

    #[test]
    fn public_post_falls_back_to_octet_stream_when_browser_mime_empty() {
        let payload = process_and_seal_public_post_inner(b"bytes", "", false);
        let sidecar = decode_sidecar(&payload.sidecar());
        assert_eq!(sidecar.mime, "application/octet-stream");
    }

    #[test]
    fn public_post_honors_browser_c2pa_detection_on_an_image() {
        let payload = process_and_seal_public_post_inner(b"fake png bytes", "image/png", true);
        let sidecar = decode_sidecar(&payload.sidecar());
        assert!(sidecar.has_c2pa);
    }

    #[test]
    fn public_post_ignores_browser_c2pa_detection_on_a_non_image() {
        // The stub `process_media` can't sniff this as an image and the
        // browser declines to override a non-empty MIME it already trusts, so
        // the final sniffed mime stays non-image — the gate must not set
        // `has_c2pa` regardless of what the browser claims.
        let payload = process_and_seal_public_post_inner(b"not an image", "text/plain", true);
        let sidecar = decode_sidecar(&payload.sidecar());
        assert!(!sidecar.has_c2pa);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_generate_and_derive() {
        let secret = generate_keypair_inner();
        assert_eq!(secret.len(), 64);
        let actor_id = actor_id_from_secret_inner(&secret).unwrap();
        assert_eq!(actor_id.len(), 64);

        // Deriving again gives the same result
        let actor_id2 = actor_id_from_secret_inner(&secret).unwrap();
        assert_eq!(actor_id, actor_id2);
    }

    #[test]
    fn test_build_register_request_roundtrip() {
        // verify-ok(test): this module signs with a locally generated key and checks
        // its own signature back — no wire-supplied key reaches it, so the permissive
        // trait is harmless here. Production verification goes through
        // `fauna_core::identity::verify_detached`; the walk guard
        // `fauna-core/tests/one_ed25519_verification_shape.rs` reads this marker.
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};

        let secret = generate_keypair_inner();
        let json_str = build_register_request_inner(&secret, "alice", "test.fauna.social").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        // Verify all expected fields are present
        let actor_id_hex = parsed["actor_id"].as_str().unwrap();
        assert_eq!(actor_id_hex.len(), 64);
        assert_eq!(parsed["handle"].as_str().unwrap(), "alice");
        let timestamp = parsed["timestamp"].as_u64().unwrap();
        assert!(timestamp > 0);
        let sig_hex = parsed["signature"].as_str().unwrap();
        assert_eq!(sig_hex.len(), 128); // 64 bytes = 128 hex

        // Reconstruct the tagged, length-prefixed register message via the
        // single-source builder — exactly what the nest verifies.
        let actor_id_bytes = hex::decode(actor_id_hex).unwrap();
        let actor_arr: [u8; 32] = actor_id_bytes.clone().try_into().unwrap();
        let msg = fauna_protocol::account::register_signed_message(
            &actor_arr,
            "alice",
            "test.fauna.social",
            timestamp,
        );

        // Verify signature
        let vk_bytes: [u8; 32] = actor_id_bytes.try_into().unwrap();
        let verifying_key = VerifyingKey::from_bytes(&vk_bytes).unwrap();
        let sig_bytes = hex::decode(sig_hex).unwrap();
        let sig_arr: [u8; 64] = sig_bytes.try_into().unwrap();
        let signature = Signature::from_bytes(&sig_arr);
        verifying_key
            .verify(&msg, &signature)
            .expect("signature must verify");
    }

    #[test]
    fn invalid_secret_rejected() {
        assert!(actor_id_from_secret_inner("not_hex").is_err());
        assert!(actor_id_from_secret_inner("aabb").is_err()); // too short
    }
}

// ── Content moderation ──────────────────────────────────────

#[wasm_bindgen]
pub fn classify_text(text: &str) -> String {
    let results = fauna_client_core::scan::classify_text(text);
    let json: Vec<serde_json::Value> = results
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "category": r.category,
                "confidence": r.confidence,
            })
        })
        .collect();
    serde_json::to_string(&json).unwrap_or_else(|_| "[]".to_string())
}

/// `fauna_core::obligation::obligation_action_label` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — the shared
/// obligation-action discriminant → label map (`action: u8` →
/// `moderation.action.*`), so a moderation-queue row renders the action, not a
/// raw number. `action` is the verbatim `fauna.moderation.actions` `action`
/// field. The companion category map is `contentLabelStyle` above.
#[wasm_bindgen(js_name = obligationActionLabel)]
pub fn obligation_action_label(action: u8) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::obligation::obligation_action_label(action))
}

/// `fauna_core::obligation::legal_takedown_tombstone` → a `LocalizedText`
/// `{ key, args }` the SPA resolves through `resolveLocalized` — the shared
/// legal-takedown tombstone ("removed under legal obligation [reference]",
/// `moderation.md` § Categories & enforcement item 1) the web feed/post view
/// renders in place of a post body once `PostGetReply.legal_takedown` is
/// present. `reference` is the legal-obligation reference the authority
/// supplied; the returned `{reference}` arg fills the `moderation.legal_takedown.tombstone`
/// template. The wasm twin of the UniFFI `legalTakedownTombstone` face.
#[wasm_bindgen(js_name = legalTakedownTombstone)]
pub fn legal_takedown_tombstone(reference: &str) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_core::obligation::legal_takedown_tombstone(reference))
}

/// `fauna_client_moderation::merge_queue` → the unified moderation-queue rows —
/// the wasm twin of the UniFFI `moderation_queue` façade the natives use
/// (`libs/fauna-ffi/src/moderation_client.rs`), so every app unions the queue's
/// two sources through the **one** shared dedupe rule (priority #2). `server` is
/// the JS array of `fauna.moderation.actions` `ObligationAction`s (from
/// `moderationActions`); `local` is the JS array of the client's own post-decrypt
/// `LocalDetection`s (`{ content_id, content_type, category, confidence_per_mille
/// (u16, 0–1000), timestamp (i64 microsecond epoch) }` — the shared per-mille /
/// micro shape, not web's legacy `0..1`-float `flaggedItems`). Returns the merged,
/// deduped-by-`content_id` (server row wins), newest-first `QueueRow`s — server
/// rows carrying the enforcement `action`, local detections a blank (`null`) action
/// column (`moderation.md` § Layout & flow). Pure — no nest hop — so a free fn, not
/// a `WsRpcClient` method; it lets the web SPA retire its hand-rolled TS union
/// (`apps/fauna-web/src/routes/settings/[[subpage]]/+page.svelte`). (Slice 5.)
#[wasm_bindgen(js_name = moderationQueue)]
pub fn moderation_queue(server: JsValue, local: JsValue) -> Result<JsValue, JsValue> {
    let server: Vec<fauna_client_moderation::moderation::ObligationAction> =
        crate::rpc::from_js(server)?;
    let local: Vec<fauna_client_moderation::LocalDetection> = crate::rpc::from_js(local)?;
    let rows = fauna_client_moderation::merge_queue(&server, &local);
    crate::rpc::to_js(&rows)
}

/// `fauna_client_moderation::takedown_form_view` → what the admin-nest
/// legal-takedown console renders (`moderation.md` § Legal takedown →
/// *Invocation surface*) — the wasm twin of the UniFFI `takedown_form_view`
/// face, so every app applies the ONE shared gating/wording (a citation-less
/// takedown is never armable; a note-less RESTORE is; the armed confirm names
/// verb + content + citation). Returns `{ can_submit, blocked_reason,
/// arm_label, confirm_summary, confirm_label }`, the text fields as
/// `LocalizedText` `{key, args}` for the SPA's i18n pipeline. Pure — no nest
/// hop — so a free fn.
#[wasm_bindgen(js_name = takedownFormView)]
pub fn takedown_form_view(
    content_id: String,
    conversation: bool,
    legal_reference: String,
    restore: bool,
) -> Result<JsValue, JsValue> {
    use fauna_client_moderation::takedown::{TakedownContentType, TakedownForm};
    let view = fauna_client_moderation::takedown_form_view(&TakedownForm {
        content_id,
        content_type: if conversation {
            TakedownContentType::Conversation
        } else {
            TakedownContentType::Post
        },
        legal_reference,
        restore,
    });
    crate::rpc::to_js(&view)
}

// ── User-initiated reporting (`moderation.md` § User-initiated reporting →
//    *Where logic lives*) — the wasm twins of the UniFFI `abuse_report.rs`
//    faces. Pure folds over the shared `fauna_client_moderation::report`, so
//    free fns; text fields are `LocalizedText` `{key, args}`.

/// `report::report_sheet_view` — `target` is `{ subject, sealed, author,
/// plaintext }` (`subject` the wire's tagged `AbuseReportSubject`), `form` is
/// `{ reason, note, include_text, block_author }` with `reason` a wire token or
/// `null`.
#[wasm_bindgen(js_name = reportSheetView)]
pub fn report_sheet_view(target: JsValue, form: JsValue) -> Result<JsValue, JsValue> {
    let target: fauna_client_moderation::report::ReportTarget = crate::rpc::from_js(target)?;
    let form: fauna_client_moderation::report::ReportForm = crate::rpc::from_js(form)?;
    crate::rpc::to_js(&fauna_client_moderation::report::report_sheet_view(
        &target.subject,
        target.sealed,
        &form,
    ))
}

/// `report::report_failed` — the send's failure line.
#[wasm_bindgen(js_name = reportFailed)]
pub fn report_failed(error: String) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_moderation::report::report_failed(error))
}

/// `report::ledger_title` / `ledger_empty` — the ledger's header and empty
/// state, as `{ title, empty }`.
#[wasm_bindgen(js_name = reportLedgerWords)]
pub fn report_ledger_words() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&serde_json::json!({
        "title": fauna_client_moderation::report::ledger_title(),
        "empty": fauna_client_moderation::report::ledger_empty(),
    }))
}

/// `report::withdraw_verdict` — `null` on success.
#[wasm_bindgen(js_name = reportWithdrawVerdict)]
pub fn report_withdraw_verdict(error: Option<String>) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_moderation::report::withdraw_verdict(error))
}

/// `ReportTarget::post` — a feed post; `gated` is the sealed rule's post arm.
#[wasm_bindgen(js_name = reportPostTarget)]
pub fn report_post_target(
    cid: String,
    author: String,
    plaintext: String,
    gated: bool,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_moderation::report::ReportTarget::post(
        &cid, &author, &plaintext, gated,
    ))
}

/// `ReportTarget::actor` — the OTHER profile.
#[wasm_bindgen(js_name = reportActorTarget)]
pub fn report_actor_target(actor_id: String) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_moderation::report::ReportTarget::actor(
        &actor_id,
    ))
}

/// `ReportTarget::message` — a conversation message off its plane ref; `null`
/// for a mail / bridged message (no verb).
#[wasm_bindgen(js_name = reportMessageTarget)]
pub fn report_message_target(
    plane_scope: String,
    record_digest: String,
    sender_actor: Option<String>,
    plaintext: String,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_moderation::report::ReportTarget::message(
        &plane_scope,
        &record_digest,
        sender_actor,
        &plaintext,
    ))
}

/// `report::message_subject` — the report subject for a conversation message
/// from its plane ref, or `null` for a mail / bridged message (no verb).
#[wasm_bindgen(js_name = reportMessageSubject)]
pub fn report_message_subject(
    plane_scope: String,
    record_digest: String,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_moderation::report::message_subject(
        &plane_scope,
        &record_digest,
    ))
}

/// `report::takedown_prefill` — `{ content_id, conversation }` for a post or
/// message subject (`subject` the wire's tagged `AbuseReportSubject`), `null`
/// for an account.
#[wasm_bindgen(js_name = reportTakedownPrefill)]
pub fn report_takedown_prefill(subject: JsValue) -> Result<JsValue, JsValue> {
    use fauna_client_moderation::takedown::TakedownContentType;
    let subject: fauna_protocol::moderation::AbuseReportSubject = crate::rpc::from_js(subject)?;
    crate::rpc::to_js(
        &fauna_client_moderation::report::takedown_prefill(&subject).map(|form| {
            serde_json::json!({
                "content_id": form.content_id,
                "conversation": form.content_type == TakedownContentType::Conversation,
            })
        }),
    )
}

/// `report::resolve_verdict` — `acted` false is a dismissal.
#[wasm_bindgen(js_name = reportResolveVerdict)]
pub fn report_resolve_verdict(acted: bool, error: Option<String>) -> Result<JsValue, JsValue> {
    use fauna_protocol::moderation::AbuseReportOutcome;
    let outcome = if acted {
        AbuseReportOutcome::Acted
    } else {
        AbuseReportOutcome::Dismissed
    };
    crate::rpc::to_js(&fauna_client_moderation::report::resolve_verdict(
        outcome, error,
    ))
}

/// `fauna_client_admin::issuer_key_row_label` → one
/// `admin-nest-oauth-key-item-{n}` line. `row` is one entry of the
/// `adminIssuerKeyStatus` view's `keys`, `now_secs` the SPA's clock at paint
/// (epoch seconds — a retired key's countdown is the point of its line). Pure,
/// so a free fn; the wasm twin of the UniFFI `issuer_key_row_label`.
#[wasm_bindgen(js_name = issuerKeyRowLabel)]
pub fn issuer_key_row_label(row: JsValue, now_secs: f64) -> Result<JsValue, JsValue> {
    let row: fauna_client_admin::IssuerKeyRow = crate::rpc::from_js(row)?;
    crate::rpc::to_js(&fauna_client_admin::issuer_key_row_label(
        &row,
        now_secs as i64,
    ))
}

/// `fauna_client_admin::issuer_key_rotate_cost` → the ordinary arm's cost,
/// painted beside `admin-nest-oauth-rotate-button` (it has no confirm).
#[wasm_bindgen(js_name = issuerKeyRotateCost)]
pub fn issuer_key_rotate_cost(view: JsValue) -> Result<JsValue, JsValue> {
    let view: fauna_client_admin::IssuerKeyView = crate::rpc::from_js(view)?;
    crate::rpc::to_js(&fauna_client_admin::issuer_key_rotate_cost(&view))
}

/// `fauna_client_admin::issuer_forced_confirm_view` → `{ summary,
/// confirm_label }` for `arm` (`"IssuerKey"` / `"SessionSecret"`), folded at
/// ARM time over the view the admin is looking at; the armed confirm renders
/// what this returned and is never re-folded while armed.
#[wasm_bindgen(js_name = issuerForcedConfirmView)]
pub fn issuer_forced_confirm_view(arm: JsValue, view: JsValue) -> Result<JsValue, JsValue> {
    let arm: fauna_client_admin::IssuerForcedArm = crate::rpc::from_js(arm)?;
    let view: fauna_client_admin::IssuerKeyView = crate::rpc::from_js(view)?;
    crate::rpc::to_js(&fauna_client_admin::issuer_forced_confirm_view(arm, &view))
}

/// `fauna_client_moderation::takedown_verdict` → the console's outcome line
/// (`admin-nest-takedown-status`) — pass the rejection's error string on
/// failure, `null` on success. The wasm twin of the UniFFI `takedown_verdict`.
#[wasm_bindgen(js_name = takedownVerdict)]
pub fn takedown_verdict(restore: bool, error: Option<String>) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_moderation::takedown_verdict(restore, error))
}

/// `fauna_client_admin::admin_user_row_controls` → `{ suspend, evict, restore,
/// make_admin, remove_admin }`: which lifecycle/role controls an admin Users row
/// offers — the wasm twin
/// of the UniFFI `admin_user_row_controls` face the natives use
/// (`libs/fauna-ffi/src/admin.rs`), so all seven apps apply the ONE shared rule
/// (priority #1/#2). The rule is three eviction states crossed with the admin-role
/// guard, and it is genuinely easy to get wrong: **Suspend stays offered on a
/// mid-eviction `warning` row** (it promotes the user and *clears* the pending
/// delete — the no-user-data-loss direction), and an **admin row offers neither
/// entry control** (the nest answers `fauna.admin.conflict`) while still offering
/// restore, so granting the role to a suspended user can't strand them. `user` is a
/// `fauna.admin.users.list` row (the SPA's `AdminUser`) — the WHOLE row: this
/// deserializes the full projection, so a partial object is a runtime error. Pure —
/// no nest hop — so a free fn, not a `WsRpcClient` method. See
/// `docs/goal/behavior/admin.md` § 2 Users → *Cutting a user off*.
#[wasm_bindgen(js_name = adminUserRowControls)]
pub fn admin_user_row_controls(user: JsValue) -> Result<JsValue, JsValue> {
    let user: fauna_protocol::admin::AdminUser = crate::rpc::from_js(user)?;
    crate::rpc::to_js(&fauna_client_admin::admin_user_row_controls(&user))
}

/// `fauna_client_admin::admin_picker_option` → the option text an admin
/// picker (guardian, invite-request) offers for `user`: the **handle**,
/// falling back to the full actor hex for a handle-less account (`admin.md`
/// § 2 → *What identifies a user in an admin picker*) — the wasm twin of the
/// UniFFI `admin_picker_option` face the natives use
/// (`libs/fauna-ffi/src/admin.rs`), so no client re-derives the handle-or-hex
/// fallback (priority #1/#2). `user` is a `fauna.admin.users.list` row (the
/// SPA's `AdminUser`) — the WHOLE row, same contract as
/// `admin_user_row_controls`. Pure — no nest hop — so a free fn.
#[wasm_bindgen(js_name = adminPickerOption)]
pub fn admin_picker_option(user: JsValue) -> Result<String, JsValue> {
    let user: fauna_protocol::admin::AdminUser = crate::rpc::from_js(user)?;
    Ok(fauna_client_admin::admin_picker_option(&user))
}

/// `fauna_client_admin::registration_mode_options` → the
/// `admin-users-registration-mode-select` picker catalog (wire value + `{key,
/// args}` localized label, in display order) — the wasm twin of the UniFFI
/// `registration_mode_options` face the natives use
/// (`libs/fauna-ffi/src/admin.rs`), so no client spells the
/// `open`/`invite_required`/`closed` vocabulary or its order itself (priority
/// #1/#2). Pure — no nest hop — so a free fn. See
/// `docs/goal/behavior/admin.md` § 2 Users → *Section 2 — Registration*.
#[wasm_bindgen(js_name = registrationModeOptions)]
pub fn registration_mode_options() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_admin::registration_mode_options())
}

/// `fauna_client_admin::age_band_options` → the two admission age-band
/// selects' catalog (`admin-users-invite-age-band-select` /
/// `invite-request-row-age-band-select`: *not set* + the four bands, wire
/// value + `{key, args}` localized label, in the ratified order) — the wasm
/// twin of the UniFFI `age_band_options` face, so no client spells
/// `u13`/`13-15`/`16-17`/`18+` or its order itself (`family-safety.md` § App
/// surface → *Age-band surfaces*). Pure — no nest hop — so a free fn.
#[wasm_bindgen(js_name = ageBandOptions)]
pub fn age_band_options() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_admin::age_band_options())
}

/// `fauna_client_admin::claimed_age_band_option` → the
/// `invite-request-row-age-band-select` seed for a pending request: the
/// applicant's claimed band when this client can name it, else the *not set*
/// value — the wasm twin of the UniFFI face.
#[wasm_bindgen(js_name = claimedAgeBandOption)]
pub fn claimed_age_band_option(claimed: Option<String>) -> String {
    fauna_client_admin::claimed_age_band_option(claimed.as_deref())
}

/// `fauna_protocol::age::age_claim_label` → the `invite-request-row-age-claim`
/// text for a request row's `{age_band, age_band_provenance}` — total ("No app
/// age verification" when no nameable claim rides the row). The args are
/// nested keys: resolve with the nested form.
#[wasm_bindgen(js_name = ageClaimLabel)]
pub fn age_claim_label(
    band: Option<String>,
    provenance: Option<String>,
) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_protocol::age::age_claim_label(
        band.as_deref(),
        provenance.as_deref(),
    ))
}

/// `fauna_protocol::age::age_band_line` → the two family-page readouts' line
/// (`family-ward-age-band` with `own = false`, `family-age-band-summary` with
/// `own = true`), or `null` when the band is unnamed (absent, never
/// placeholdered). Nested-key args.
#[wasm_bindgen(js_name = ageBandLine)]
pub fn age_band_line(band: String, provenance: String, own: bool) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_protocol::age::age_band_line(&band, &provenance, own))
}

/// `fauna_protocol::age::age_band_label` → a band's display label for a wire
/// token, or `null` when this client cannot name it — the minted band's echo
/// on an `invite-code-item` row (same row, richer text, no new id). The wasm
/// twin of the UniFFI `age_band_label` face.
#[wasm_bindgen(js_name = ageBandLabel)]
pub fn age_band_label(band: String) -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_protocol::age::age_band_label(&band))
}

// `build_scan_report` (the client-signed scan-report builder) and, later, the
// `WsRpcClient.moderationScanReport` wrapper were both removed: the
// `fauna.moderation.scan_report` kind left the wire 2026-09-24 — its client-side
// producer was retired 2026-07-19 (`moderation.md` § State & data shape).

// The client-side Bayesian spam filter (`bayes_import`/`bayes_export`/
// `bayes_train_spam`/`bayes_train_ham`/`bayes_score`/`bayes_reset`, wrapping
// the retired `bayespam` crate) was removed — it had zero production callers
// (only a Settings-page Export/Import round-trip); the real on-device
// "Fauna-app" scoring position (`docs/goal/behavior/mail-spam.md` § Scoring
// placement) is `libs/fauna-client-mail-settings/src/inbox_scorer.rs`,
// scoring against the shared `fauna_mail::spam::SpamModel`.
