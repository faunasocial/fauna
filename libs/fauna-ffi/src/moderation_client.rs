//! UniFFI façade for the client-facing `fauna.moderation.*` WS-RPC kinds —
//! the queue read (`actions`), the training correction (`train`), and the
//! distributed report-/signal-sharing opt-ins.
//!
//! [`FfiModerationClient`] wraps `fauna_client_moderation::ModerationClient`
//! (which in turn wraps the shared `NestClient`). The Rust-native Linux app
//! calls the same `ModerationClient` directly — this seam gives Android (and
//! windows / apple) the identical surface over UniFFI (priority #2).
//!
//! `scan_report` / `FfiScanLabel` were removed 2026-07-19 (the client-side
//! compliance-beacon producer was retired without replacement — `moderation.md`
//! § State & data shape owns the verdict); the kind itself left the wire
//! 2026-09-24 with the compat-remnant sweep.
//!
//! Built-in types only over the FFI boundary, so this stays available in the Go
//! mail-bridge `--no-default-features` build, the same as the sibling
//! `inbox_client` seam.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_moderation::ModerationClient;
use fauna_client_moderation::moderation::{ObligationAction, ReportShareEntry};
#[cfg(feature = "moderation-badge")]
use fauna_client_moderation::{LocalDetection, QueueRow, merge_queue};

use crate::{FfiError, stringify};

// ── ObligationAction mirror ─────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::moderation::ObligationAction`] — one
/// moderation-queue row: why a piece of the caller's content was labeled /
/// quarantined / rejected. `content_type` + `content_id` reference the item;
/// `category` is the classifier category (`spam`/`phishing`/…, rendered via the
/// shared `contentLabelStyle` map); `confidence_per_mille` is `0..=1000`;
/// `action` is the raw obligation-action discriminant (rendered via the shared
/// `obligationActionLabel` map, never raw); `timestamp` is a **microsecond**
/// epoch. The wire's `extra` forward-compat catch-all is dropped (a decode
/// escape hatch, never a value the UI reads). Built-in fields only, so this
/// stays in the Go mail-bridge `--no-default-features` build.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiObligationAction {
    pub id: i64,
    pub content_type: String,
    pub content_id: String,
    pub category: String,
    pub confidence_per_mille: u16,
    pub action: u8,
    pub timestamp: i64,
}

impl From<ObligationAction> for FfiObligationAction {
    fn from(a: ObligationAction) -> Self {
        FfiObligationAction {
            id: a.id,
            content_type: a.content_type,
            content_id: a.content_id,
            category: a.category,
            confidence_per_mille: a.confidence_per_mille,
            action: a.action,
            timestamp: a.timestamp,
        }
    }
}

// ── ReportShareEntry mirror ─────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::moderation::ReportShareEntry`] — one
/// published report aggregate the transparency pane renders (`report-sharing.md`
/// § Client wire + transparency surface). Every entry has passed the
/// k-anonymity gate and is exactly what this nest exports to peers: a
/// `(content_hash, factor, count)` triple, `count` a local reporter count ≥ k,
/// never a reporter identity. `content_hash` is hex-encoded 32 bytes. Built-in
/// fields only, so this stays in the Go mail-bridge `--no-default-features`
/// build; the wire's `extra` forward-compat catch-all is dropped (a decode
/// escape hatch, never a value the UI reads).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiReportShareEntry {
    pub content_hash: String,
    pub factor: String,
    pub count: u32,
}

impl From<ReportShareEntry> for FfiReportShareEntry {
    fn from(e: ReportShareEntry) -> Self {
        FfiReportShareEntry {
            content_hash: e.content_hash,
            factor: e.factor,
            count: e.count,
        }
    }
}

/// FFI shape of `fauna.moderation.report_share.status`'s reply — the caller's
/// opt-in state (`share`) plus the transparency list (`published`, one
/// [`FfiReportShareEntry`] per ≥k aggregate this nest exports). A dedicated
/// record rather than a tuple (UniFFI has no tuple type). Built-in fields only,
/// so it stays in the Go mail-bridge `--no-default-features` build.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiReportShareStatus {
    pub share: bool,
    pub published: Vec<FfiReportShareEntry>,
}

// ── FfiModerationClient ─────────────────────────────────────────────────

/// UniFFI handle for the `fauna.moderation.{actions,train}` + report-/signal-
/// sharing kinds.
/// Construct via [`crate::nest_client::FfiNestClient::moderation`]; the methods
/// are exposed to Swift as `async throws` and Kotlin as `suspend fun`.
/// Caller-scoped by construction (no `actor_id` param — the connection knows
/// its caller).
#[derive(uniffi::Object)]
pub struct FfiModerationClient {
    nest: Arc<NestClient>,
}

impl FfiModerationClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    pub(crate) fn client(&self) -> ModerationClient<Arc<NestClient>> {
        ModerationClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiModerationClient {
    /// `fauna.moderation.actions` — read the caller's own moderation queue (one
    /// [`FfiObligationAction`] per flagged/actioned item). Pure read; the queue
    /// may be empty (→ the client shows its empty state). The connection scopes
    /// to its caller, so no `actor_id` param. This is the queue-read leg the
    /// Apple / Windows standalone Moderation page renders (linux consumes the
    /// same `ModerationClient::actions()` natively).
    pub async fn actions(&self) -> Result<Vec<FfiObligationAction>, FfiError> {
        let reply = self.client().actions().await.map_err(stringify)?;
        Ok(reply.actions.into_iter().map(Into::into).collect())
    }

    /// `fauna.moderation.train` — submit a spam/ham training correction for one
    /// stored post. `content_id` is the hex 32-byte post id; `verdict` must be
    /// `"spam"` or `"ham"` (the queue's per-row correction marks a flagged item
    /// not-spam = `"ham"`). The reply (`{ retrained }`) is dropped; only the
    /// error channel matters to the caller.
    pub async fn train(&self, content_id: String, verdict: String) -> Result<(), FfiError> {
        self.client()
            .train(content_id, verdict)
            .await
            .map_err(stringify)?;
        Ok(())
    }

    /// `fauna.moderation.report_share.set` — set the caller's distributed
    /// report-sharing opt-in (`report-sharing.md` § Client wire; default off).
    /// Caller-scoped (no `actor_id` — the connection knows its caller), so this
    /// flips only the caller's own preference. `share=false` also withdraws
    /// every report the caller contributed (nest-side opt-out sweep). Returns
    /// the state now in effect (echoes `share`) so the toggle can confirm.
    pub async fn report_share_set(&self, share: bool) -> Result<bool, FfiError> {
        let reply = self
            .client()
            .report_share_set(share)
            .await
            .map_err(stringify)?;
        Ok(reply.share)
    }

    /// `fauna.moderation.report_share.status` — read the caller's opt-in state
    /// **and** the transparency list of what this nest publishes to peers. The
    /// pane renders the toggle from `.share` and the published list from
    /// `.published` (one [`FfiReportShareEntry`] per ≥k aggregate, identical to
    /// the federation export). Pure read; the list may be empty.
    pub async fn report_share_status(&self) -> Result<FfiReportShareStatus, FfiError> {
        let reply = self
            .client()
            .report_share_status()
            .await
            .map_err(stringify)?;
        Ok(FfiReportShareStatus {
            share: reply.share,
            published: reply.published.into_iter().map(Into::into).collect(),
        })
    }

    /// `fauna.moderation.signal_share.set` — set the caller's Layer-B
    /// engagement-signal-sharing opt-in (`engagement-cues.md` § Layer B; default
    /// off, INDEPENDENT of report sharing). Caller-scoped. `share=false` also
    /// withdraws every `signal:*` contribution the caller made (nest-side
    /// factor-scoped opt-out sweep — never touches their `report:spam` rows).
    /// Returns the state now in effect so the toggle can confirm.
    pub async fn signal_share_set(&self, share: bool) -> Result<bool, FfiError> {
        let reply = self
            .client()
            .signal_share_set(share)
            .await
            .map_err(stringify)?;
        Ok(reply.share)
    }

    /// `fauna.moderation.signal_share.status` — read the caller's signal opt-in
    /// state **and** the transparency list. `.published` is the same export view
    /// `report_share_status` returns (one [`FfiReportShareEntry`] per ≥k
    /// aggregate the nest publishes, `report:*` and `signal:*` alike). Pure read;
    /// the list may be empty. Reuses [`FfiReportShareStatus`] (identical shape).
    pub async fn signal_share_status(&self) -> Result<FfiReportShareStatus, FfiError> {
        let reply = self
            .client()
            .signal_share_status()
            .await
            .map_err(stringify)?;
        Ok(FfiReportShareStatus {
            share: reply.share,
            published: reply.published.into_iter().map(Into::into).collect(),
        })
    }

    /// `fauna.moderation.signal_contribute` — contribute one derived cue verdict
    /// for a **public** post (`engagement-cues.md` § Layer B write path). Honored
    /// only when the caller opted in, except `signal = "withdraw"` which always
    /// applies. `content_id` is the hex 32-byte post id; `signal` is
    /// `"watch-complete"`, `"skip"`, or `"withdraw"`. The reply status is
    /// dropped; only the error channel matters to the fire-and-forget caller.
    pub async fn signal_contribute(
        &self,
        content_id: String,
        signal: String,
    ) -> Result<(), FfiError> {
        self.client()
            .signal_contribute(content_id, signal)
            .await
            .map_err(stringify)?;
        Ok(())
    }
}

// ── Obligation-action label ──────────────────────────────────────────────
//
// The companion content-label *category* map is the shared
// `fauna_core::content_category::content_label_style` (UniFFI `contentLabelStyle`
// in `lib.rs`); the action-label map lives next to its enum in
// `fauna_core::obligation`. Both moderation-badge presentation maps are in
// fauna-core (priority #4 — one home), so this seam only wraps the action one.

/// UniFFI face of [`fauna_core::obligation::obligation_action_label`] — the
/// shared obligation-action discriminant → label map (the `action: u8` a
/// `fauna.moderation.actions` row carries → `moderation.action.*`), so a queue
/// row renders "Quarantined" / "Labeled" instead of a raw `u8`, returned as a
/// [`LocalizedText`](fauna_core::localized::LocalizedText) each app resolves
/// through its own i18n runtime.
///
/// Gated behind `moderation-badge` — the same feature as its category sibling
/// `contentLabelStyle` (so both moderation presentation faces toggle together,
/// and the gate is self-contained: `moderation-badge` forwards `fauna-core/uniffi`,
/// which registers the `LocalizedText` this returns). A bare `fauna_core::Localized-
/// Text` crosses the boundary, which `uniffi-bindgen-go` emits as an uncompilable
/// cross-namespace import — but the Go mail-bridge's `--no-default-features` build
/// drops it (no moderation queue UI there, so harmless → no `mail-bridge-ffi`
/// regen), same Go-incompatibility rationale as `value-format` / `nostr_key_source_label`.
#[cfg(feature = "moderation-badge")]
#[uniffi::export]
pub fn obligation_action_label(action: u8) -> fauna_core::localized::LocalizedText {
    fauna_core::obligation::obligation_action_label(action)
}

/// UniFFI face of [`fauna_core::obligation::legal_takedown_tombstone`] — the
/// shared legal-takedown tombstone text ("removed under legal obligation
/// [reference]", `moderation.md` § Categories & enforcement item 1) a client
/// renders in place of a post's body once `PostGetReply.legal_takedown` is
/// present. Returns a [`LocalizedText`](fauna_core::localized::LocalizedText)
/// (key `moderation.legal_takedown.tombstone` + `{reference}` arg) each app
/// resolves through its own i18n runtime — no client hand-rolls the string.
/// Same `moderation-badge` gate / Go-drop rationale as `obligation_action_label`.
#[cfg(feature = "moderation-badge")]
#[uniffi::export]
pub fn legal_takedown_tombstone(reference: &str) -> fauna_core::localized::LocalizedText {
    fauna_core::obligation::legal_takedown_tombstone(reference)
}

// ── Moderation-queue union ────────────────────────────────────────────────

/// UniFFI face of [`fauna_client_moderation::merge_queue`] — the one shared
/// moderation-queue union used by every native app (apple / windows / android),
/// so the server-∪-local dedupe rule never drifts per client (priority #2; linux
/// calls `merge_queue` in-process). A client fetches its server rows via
/// [`FfiModerationClient::actions`] and its post-decrypt local detections via
/// `ConversationsSession::moderation_local_detections`, then passes both here to get
/// the merged, deduped, newest-first [`QueueRow`] list its queue VM renders — server
/// rows carrying the enforcement `action`, local detections a **blank** action column
/// (`docs/goal/behavior/moderation.md` § Layout & flow). Dedupe is by `content_id`,
/// the server row winning.
///
/// Gated behind `moderation-badge` (the same moderation-queue presentation gate as
/// `obligation_action_label`), so it is dropped from the Go mail-bridge's
/// `--no-default-features` build — the bridge has no moderation queue UI, so no
/// `mail-bridge-ffi` regen for this fn (the `QueueRow` / `LocalDetection` records it
/// uses ride the always-on `fauna-conversations/uniffi` → `fauna-client-moderation/uniffi`
/// registration, since the session reader already returns them).
#[cfg(feature = "moderation-badge")]
#[uniffi::export]
pub fn moderation_queue(
    server: Vec<FfiObligationAction>,
    local: Vec<LocalDetection>,
) -> Vec<QueueRow> {
    let server: Vec<ObligationAction> = server
        .into_iter()
        .map(|a| ObligationAction {
            id: a.id,
            content_type: a.content_type,
            content_id: a.content_id,
            category: a.category,
            confidence_per_mille: a.confidence_per_mille,
            action: a.action,
            timestamp: a.timestamp,
            extra: Default::default(),
        })
        .collect();
    merge_queue(&server, &local)
}

// ── Legal-takedown console (moderation.md § Legal takedown → Invocation
//    surface) ─────────────────────────────────────────────────────────────
//
// Gated `moderation-badge` for the SAME reason as `content_category.rs`: the
// view embeds bare `fauna_core::LocalizedText`, which `uniffi-bindgen-go`
// can't emit as a cross-namespace import — and the Go mail-bridge has no
// takedown surface, so the whole console section (dispatch included) stays out
// of its `--no-default-features` build and the tracked Go binding needs no
// regen for it.

/// UniFFI mirror of [`fauna_client_moderation::takedown::TakedownFormView`] —
/// what the admin-nest legal-takedown console renders, derived once in shared
/// Rust so no app grows its own gating/wording (the four decisions the module
/// doc there enumerates: the citation guard, its restore asymmetry, the named
/// confirm, the verdict split).
#[cfg(feature = "moderation-badge")]
#[derive(uniffi::Record)]
pub struct FfiTakedownFormView {
    /// Whether the arm control may be offered.
    pub can_submit: bool,
    /// Why not, when it may not.
    pub blocked_reason: Option<fauna_core::localized::LocalizedText>,
    /// The arm control's label (verb flips with restore).
    pub arm_label: fauna_core::localized::LocalizedText,
    /// The armed confirm's decision surface (names verb + content + citation).
    pub confirm_summary: fauna_core::localized::LocalizedText,
    /// The confirm control's label.
    pub confirm_label: fauna_core::localized::LocalizedText,
}

/// The console's per-keystroke fold — `conversation` selects the MLS
/// relay-withhold kind (`content_type = "conversation"`), else `"post"`.
#[cfg(feature = "moderation-badge")]
#[uniffi::export]
pub fn takedown_form_view(
    content_id: String,
    conversation: bool,
    legal_reference: String,
    restore: bool,
) -> FfiTakedownFormView {
    use fauna_client_moderation::takedown::{self, TakedownContentType, TakedownForm};
    let view = takedown::takedown_form_view(&TakedownForm {
        content_id,
        content_type: if conversation {
            TakedownContentType::Conversation
        } else {
            TakedownContentType::Post
        },
        legal_reference,
        restore,
    });
    FfiTakedownFormView {
        can_submit: view.can_submit,
        blocked_reason: view.blocked_reason,
        arm_label: view.arm_label,
        confirm_summary: view.confirm_summary,
        confirm_label: view.confirm_label,
    }
}

/// The console's outcome line (`admin-nest-takedown-status`) — pass the
/// transport/handler error string on failure, `None` on success.
#[cfg(feature = "moderation-badge")]
#[uniffi::export]
pub fn takedown_verdict(
    restore: bool,
    error: Option<String>,
) -> fauna_core::localized::LocalizedText {
    fauna_client_moderation::takedown::takedown_verdict(restore, error)
}

/// The dispatch leg of the console, on the same client handle the queue read
/// uses. Behind `moderation-badge` with the views above so the whole console
/// surface ships (or not) as one unit.
#[cfg(feature = "moderation-badge")]
#[fauna_uniffi_async::export]
impl FfiModerationClient {
    /// `fauna.moderation.legal_takedown` — the Admin-only legal-compulsion
    /// takedown / overturn. `conversation` selects the relay-withhold kind;
    /// `restore=true` overturns (reference becomes the optional note). Returns
    /// the reply's `status` (`"taken_down"` / `"restored"`); errors surface on
    /// the error channel for [`takedown_verdict`] to word.
    pub async fn legal_takedown(
        &self,
        content_id: String,
        conversation: bool,
        legal_reference: String,
        restore: bool,
    ) -> Result<String, FfiError> {
        let content_type = if conversation { "conversation" } else { "post" };
        let reply = self
            .client()
            .legal_takedown(content_id, content_type, legal_reference, restore)
            .await
            .map_err(stringify)?;
        Ok(reply.status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_share_entry_maps_from_proto() {
        // `extra` (the wire forward-compat catch-all) has no mirror field, so
        // the `From` impl drops it — the pane row carries only the built-in
        // columns it renders (all Go-compatible).
        let proto = ReportShareEntry {
            content_hash: "ab".repeat(32),
            factor: "report:spam".into(),
            count: 5,
            ..Default::default()
        };
        let ffi: FfiReportShareEntry = proto.into();
        assert_eq!(ffi.content_hash, "ab".repeat(32));
        assert_eq!(ffi.factor, "report:spam");
        assert_eq!(ffi.count, 5);
    }

    #[test]
    fn obligation_action_maps_from_proto() {
        // `extra` (the wire's forward-compat catch-all) has no mirror field, so
        // the `From` impl drops it by construction — the queue row carries only
        // the built-in columns the UI renders.
        let proto = ObligationAction {
            id: 7,
            content_type: "post".into(),
            content_id: "ab".repeat(32),
            category: "spam".into(),
            confidence_per_mille: 880,
            action: 2,
            timestamp: 1_700_000_000_000_000,
            ..Default::default()
        };
        let ffi: FfiObligationAction = proto.into();
        assert_eq!(ffi.id, 7);
        assert_eq!(ffi.content_type, "post");
        assert_eq!(ffi.content_id, "ab".repeat(32));
        assert_eq!(ffi.category, "spam");
        assert_eq!(ffi.confidence_per_mille, 880);
        assert_eq!(ffi.action, 2);
        assert_eq!(ffi.timestamp, 1_700_000_000_000_000);
    }
}
