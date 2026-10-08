//! Shared orchestration for the user-facing `mail-export` wizard (the per-account
//! mailbox-export surface): a five-step wizard (Format → Scope → Confirm →
//! Progress → Done) that pulls the user's mail-area state out in one of three
//! MUA-portable formats (mbox / Maildir++ / EML-zip), sealed-blob delivered.
//!
//! Authority for behavior: `docs/goal/behavior/mail-export.md` § UX shape
//! (the five wizard steps), § Session row model (the `export_sessions` state
//! machine `running` ↔ `paused` → `completed`/`cancelled`/`errored`), § Wire
//! shapes (`{start,list,pause,resume,cancel,finalize}_export_session` +
//! `discard_export_blob`). Authority for UX/IDs: `tests/e2e-unified/ui.yaml`
//! `mail-export` page + `mail-export-mailbox-progress-list` component.
//!
//! Mirrors `forwarders.rs` / `aliases.rs` / `spam.rs`: a [`MailExportSnapshot`]
//! the per-app UI renders + a [`MailExportAction`] surface it dispatches, over
//! one WS-RPC seam ([`MailExportNest`]). linux is the lead app; the other five
//! lift this shape (priority #2/#4).
//!
//! # The wizard is a client-side FSM over a backend session
//!
//! Steps 1–3 (Format / Scope / Confirm) are **pure client-side state** — the
//! machine drives the `step` transitions, the format pick, the scope multi-select
//! and default selection, and the Next/Back navigation with no nest round-trip.
//! Only the **durable commit** (Confirm → [`MailExportAction::Start`] →
//! `start_export_session`) and the live session controls (Pause / Resume / Cancel
//! / Discard / progress polling) touch the seam. So the wizard chrome works the
//! same whether or not the backend exists; that's the FSM the five app lifts
//! reuse.
//!
//! # The pipeline the wizard drives
//!
//! `Start` is the durable commit, and [`MailExportMachine::run_export`] is the
//! chunk-relay loop behind it (§ Export pipeline — conversion runs on the
//! CLIENT, unconditionally; the nest is a relay that holds no key and sees no
//! plaintext). One page at a time: fetch the mailbox's next sealed records,
//! open each under the account's own mail keys, serialize it into the single
//! zip-inside-zstd stream, cut the completed byte slices off that stream, seal
//! each slice under the per-session key and upload it. Then the terminator
//! frame and `finalize_export_session`.
//!
//! Three properties are load-bearing enough to name here, because each one has
//! a natural implementation that quietly breaks it:
//!
//! - **Peak memory is one message plus one chunk**, not one export — the
//!   ceiling is 10 GiB (§ Quota composition), so nothing may buffer the run.
//! - **Nothing is skipped.** A record that will not open fails the *session*
//!   (§ An unopenable record fails the session). The receive path's ratified
//!   rule is the opposite, and copying it here would hand the user an archive
//!   that is silently short.
//! - **The mailbox order is the nest's listing order** and the within-mailbox
//!   order is ascending UID — § Container shape's total order, which
//!   [`fauna_mail::export::ExportSerializer`] refuses to have broken. A client
//!   that re-sorts for display must still export in the listing order.
//!
//! The wizard's client-side steps (Format → Scope → Confirm navigation) still
//! touch no seam, so they keep working whatever the backend answers.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_core::localized::LocalizedText;
use fauna_protocol::MaybeSendSync;
use serde::{Deserialize, Serialize};

use crate::error::{DispatchError, NestError};

/// The export file format the wizard's step 1 picks — `mail-export-format-picker`.
///
/// **A re-export, not a declaration** (collapsed 2026-09-21). The canonical
/// definition is [`fauna_mail::export::ExportFormat`], which is where the
/// serializers that consume it live; this crate depends on `fauna-mail` and
/// never the reverse, so that is the layer the type belongs to. The wizard and
/// the serializer therefore speak the *same* enum end to end — no mapping, and
/// deliberately no `From`, which is what would let the two grow apart again
/// (the definition's own doc comment records why).
pub use fauna_mail::export::ExportFormat;

/// Canonical label for an [`ExportFormat`], returned as [`LocalizedText`] so each
/// app resolves it through its own i18n runtime (mirrors
/// [`member_status_label`](crate::member_status_label)). Lifts the identical
/// three-arm map that linux/windows/apple each hard-coded for the
/// `mail-export-format-picker` summary text (priority #1/#2/#4).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn export_format_label(format: ExportFormat) -> LocalizedText {
    match format {
        ExportFormat::Mbox => LocalizedText::key("mail_export.format_mbox"),
        ExportFormat::MaildirPlus => LocalizedText::key("mail_export.format_maildir"),
        ExportFormat::EmlZip => LocalizedText::key("mail_export.format_eml"),
    }
}

/// Which wizard screen is showing — drives which `mail-export-*` element subset
/// the per-app UI reveals (one shared element set, shown conditionally by
/// step, same pattern as `mail-add-credential`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ExportStep {
    /// Step 1 — `mail-export-format-picker`.
    Format,
    /// Step 2 — the `mail-export-scope-*` controls.
    Scope,
    /// Step 3 — `mail-export-confirm-summary` + `mail-export-start-button`.
    Confirm,
    /// Step 4 — `mail-export-progress-*` + the mailbox-progress list + error log.
    Progress,
    /// Step 5 — `mail-export-done-summary` + download / discard.
    Done,
}

/// The `export_sessions.state` the progress/done steps render (`mail-export.md`
/// § Session row model).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ExportSessionState {
    Running,
    Paused,
    Errored,
    Completed,
    Cancelled,
}

/// One source mailbox the scope step (`mail-export-scope-mailboxes`) offers, with
/// its current selection state. Default selection per `mail-export.md` § UX shape:
/// `Trash`/`Junk` pre-deselected, everything else pre-selected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailboxOption {
    pub name: String,
    pub selected: bool,
}

/// One per-mailbox progress row (`mail-export-mailbox-progress-list-item`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailboxProgressView {
    /// `…-list-item-name`.
    pub name: String,
    pub exported: u32,
    pub total: u32,
}

/// A live/finished export session as the seam reports it (projected from the
/// `export_sessions` wire row). The machine reads it to drive the Progress +
/// Done steps.
///
/// Deliberately **not** carrying `mailbox_progress`: the nest tracks aggregate
/// counters only, so a per-mailbox field here could only ever arrive empty and
/// would clobber the rows the drive loop maintains. The loop owns
/// [`MailExportSnapshot::mailbox_progress`] directly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ExportSessionView {
    /// The nest-minted session UUID (text form). An internal handle — the UI
    /// never displays it (`mail-export.md` § Architectural rules: "the session_id
    /// is sensitive"); the machine carries it to drive the pipeline and the
    /// pause/resume/cancel controls.
    pub session_id: String,
    pub state: ExportSessionState,
    pub format: ExportFormat,
    pub exported_count: u32,
    pub skipped_count: u32,
    pub errored_count: u32,
    pub total_count: u32,
    /// `export_sessions.error_reason` — the session-fatal reason when
    /// `state == Errored`, empty otherwise. Rendered as the single line of
    /// `mail-export-error-log`: under § An unopenable record fails the session
    /// there are no per-message skip lines to accumulate, because nothing is
    /// skipped.
    pub error_reason: String,
    /// Populated at completion (`mail-export-done-summary`).
    pub blob_bytes: Option<u64>,
    /// The actor-authed download path (`mail-export-download-url`); empty until
    /// the session completes.
    pub download_url: String,
    /// The per-session key, wrapped under the owning actor's key (§ Key
    /// material) — what the download leg unwraps to open the blob's frames.
    ///
    /// Carried on the *view*, not kept in the run's memory, because § Download
    /// flow's client is not necessarily the one that produced the archive: a
    /// second device of the same user, or the same device after a restart, has
    /// a completed session and a download URL and nothing else. The nest stores
    /// it verbatim for exactly that reason and hands it to any client of the
    /// owner. Empty for a row whose key this nest never received.
    #[serde(default)]
    pub wrapped_session_key: Vec<u8>,
    /// The session's current stream generation (`mail-export.md` § Resume): 0
    /// from `Start`, + 1 per cold resume. A run carries the generation it was
    /// opened under on every call it makes, which is how a run another device
    /// has restarted over learns it is no longer the driver.
    #[cfg_attr(feature = "uniffi", uniffi(default = 0))]
    #[serde(default)]
    pub stream_generation: u64,
    /// The scope the wizard committed, decoded from the row's
    /// `scope_descriptor`. A cold resume rebuilds the run from it — the app
    /// that restarts an export is by definition not the one whose wizard chose
    /// the mailboxes. `None` for a row whose descriptor this client cannot
    /// read, which a cold resume refuses rather than guess at.
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    #[serde(default)]
    pub scope: Option<ExportScope>,
}

/// The scope the wizard's step 2 builds, passed to `start_export_session` (the
/// glue serializes it to the `scope_descriptor` DAG-CBOR blob).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ExportScope {
    /// The selected source mailboxes (the deselected ones are dropped).
    pub mailboxes: Vec<String>,
    /// Optional "since" (`mail-export-scope-date-from`; empty = unset).
    pub date_from: String,
    /// Optional "until" (`mail-export-scope-date-to`).
    pub date_to: String,
    /// `mail-export-scope-strip-headers-toggle` — strip the transit headers
    /// (`mail-export.md` § UX shape step 2 owns the list).
    pub strip_headers: bool,
}

/// The scope's date range, resolved to the instants the drive loop compares
/// INTERNALDATEs against (`mail-export.md` § UX shape step 2 owns the rule).
///
/// **A bare date names a whole UTC day, and both ends are inclusive**: `since`
/// admits from that day's first second, `until` through that day's last. The
/// upper bound is therefore held as the *next* day's midnight, exclusive — a
/// message at 23:59:59 on the `until` day is in range, which is what a user
/// typing that date means and what an end held at the day's own midnight would
/// silently deny them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ExportDateRange {
    /// Inclusive lower bound, epoch seconds. `None` = unbounded.
    since: Option<i64>,
    /// EXCLUSIVE upper bound, epoch seconds — midnight after the `until` day.
    until_exclusive: Option<i64>,
}

impl ExportDateRange {
    /// Resolve a scope's two date strings. Empty is unbounded; anything else
    /// must be a strict `YYYY-MM-DD` naming a real day, and `since` may not
    /// fall after `until`. ⚠ A malformed or inverted range is an ERROR, never
    /// "no range": silently exporting everything in its place is exactly the
    /// under-delivering control this type exists to end.
    fn from_scope(scope: &ExportScope) -> Result<Self, DispatchError> {
        let day = |label: &str, value: &str| -> Result<Option<i64>, DispatchError> {
            if value.is_empty() {
                return Ok(None);
            }
            fauna_core::caltime::days_from_ymd(value)
                .map(Some)
                .ok_or_else(|| {
                    DispatchError::InvalidState(format!(
                        "the export's {label} date must be a real date written YYYY-MM-DD"
                    ))
                })
        };
        let since = day("since", &scope.date_from)?;
        let until = day("until", &scope.date_to)?;
        if let (Some(s), Some(u)) = (since, until)
            && s > u
        {
            return Err(DispatchError::InvalidState(
                "the export's since date is after its until date".into(),
            ));
        }
        Ok(Self {
            since: since.map(|d| d * 86_400),
            until_exclusive: until.map(|d| (d + 1) * 86_400),
        })
    }

    fn is_bounded(&self) -> bool {
        self.since.is_some() || self.until_exclusive.is_some()
    }

    fn admits(&self, internal_date: i64) -> bool {
        self.since.is_none_or(|s| internal_date >= s)
            && self.until_exclusive.is_none_or(|u| internal_date < u)
    }
}

/// Coarse machine status for spinner / disabled-control rendering.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ExportStatus {
    Idle,
    Loading,
    Working,
}

/// Read-only snapshot the per-app UI renders for `mail-export`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailExportSnapshot {
    pub step: ExportStep,
    pub format: ExportFormat,
    // ── scope (step 2) ─────────────────────────────────────────────────
    pub mailboxes: Vec<MailboxOption>,
    pub date_from: String,
    pub date_to: String,
    pub strip_headers: bool,
    // ── progress / done (steps 4–5; from the session) ──────────────────
    pub session_state: Option<ExportSessionState>,
    pub exported_count: u32,
    pub skipped_count: u32,
    pub errored_count: u32,
    pub total_count: u32,
    pub mailbox_progress: Vec<MailboxProgressView>,
    pub error_log: Vec<String>,
    pub blob_bytes: Option<u64>,
    pub download_url: String,
    /// Where the downloaded archive was saved, once [`MailExportAction::
    /// Download`] has run — the user-visible location, empty before that.
    ///
    /// On the snapshot rather than only in a toast because the Done step has to
    /// keep saying where the file went: a user who navigates away and back, or
    /// who reads the screen a minute later, gets the same answer. It is a
    /// *local* path, so it crosses no wire and is not the sensitive session id.
    pub saved_archive_path: String,
    pub status: ExportStatus,
    /// Last action's error, surfaced via `error-message`. On an app built
    /// without key custody (one whose glue does not yet spawn [`
    /// MailExportMachine::run_export`]) `Start` surfaces its honest refusal
    /// here rather than opening a session nothing would drive.
    pub error: Option<String>,
}

impl MailExportSnapshot {
    fn empty() -> Self {
        Self {
            step: ExportStep::Format,
            format: ExportFormat::Mbox,
            mailboxes: Vec::new(),
            date_from: String::new(),
            date_to: String::new(),
            strip_headers: false,
            session_state: None,
            exported_count: 0,
            skipped_count: 0,
            errored_count: 0,
            total_count: 0,
            mailbox_progress: Vec::new(),
            error_log: Vec::new(),
            blob_bytes: None,
            download_url: String::new(),
            saved_archive_path: String::new(),
            status: ExportStatus::Idle,
            error: None,
        }
    }

    /// The scope the current selections describe (the seam's `start` argument).
    fn scope(&self) -> ExportScope {
        ExportScope {
            mailboxes: self
                .mailboxes
                .iter()
                .filter(|m| m.selected)
                .map(|m| m.name.clone())
                .collect(),
            date_from: self.date_from.clone(),
            date_to: self.date_to.clone(),
            strip_headers: self.strip_headers,
        }
    }

    /// Apply a session's live state to the progress/done fields + derive the step.
    ///
    /// `mailbox_progress` is untouched: the nest has no per-mailbox data, so
    /// the drive loop is its only writer and a refresh must not wipe it.
    /// `error_log` is *set* from the row's fatal reason when there is one and
    /// left alone when there isn't, which keeps a repeated refresh idempotent
    /// instead of appending the same line each time.
    fn apply_session(&mut self, s: ExportSessionView) {
        self.session_state = Some(s.state);
        self.exported_count = s.exported_count;
        self.skipped_count = s.skipped_count;
        self.errored_count = s.errored_count;
        self.total_count = s.total_count;
        if !s.error_reason.is_empty() {
            self.error_log = vec![s.error_reason];
        }
        self.blob_bytes = s.blob_bytes;
        self.download_url = s.download_url;
        self.step = match s.state {
            ExportSessionState::Completed => ExportStep::Done,
            // running / paused / errored all render the Progress screen; the
            // controls (pause/resume) gate on `session_state`.
            _ => ExportStep::Progress,
        };
    }

    /// Return the wizard to step 1, preserving the loaded mailbox options
    /// (cancel / discard re-arm a fresh run without re-fetching).
    fn reset_to_format(&mut self) {
        let mailboxes = std::mem::take(&mut self.mailboxes);
        *self = MailExportSnapshot::empty();
        self.mailboxes = mailboxes;
    }
}

/// Default mailbox selection per `mail-export.md` § UX shape step 2: `Trash` /
/// `Junk` start deselected; everything else (INBOX / Sent / Archive / Drafts /
/// custom) selected.
fn default_selected(name: &str) -> bool {
    !matches!(name.to_ascii_lowercase().as_str(), "trash" | "junk")
}

/// Actions the per-app UI dispatches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum MailExportAction {
    /// Page load / resume: load the available mailboxes + the storage mode, and
    /// (if one exists) jump to the active session's Progress/Done screen.
    Refresh,
    /// Step 1 — pick the export format.
    SelectFormat { format: ExportFormat },
    /// Step 2 — toggle one mailbox's selection.
    ToggleMailbox { mailbox: String },
    /// Step 2 — set the "since" date (empty clears).
    SetDateFrom { value: String },
    /// Step 2 — set the "until" date (empty clears).
    SetDateTo { value: String },
    /// Step 2 — strip the transit headers.
    SetStripHeaders { on: bool },
    /// Advance Format → Scope → Confirm (client-side; no nest round-trip).
    Next,
    /// Go back Confirm → Scope → Format.
    Back,
    /// Step 3 — durable commit: open the `export_sessions` row
    /// (`start_export_session`) and move to the Progress screen.
    Start,
    /// Step 4 — pause the running session.
    Pause,
    /// Step 4 — resume the paused session.
    Resume,
    /// Step 4 — cancel + unlink the partial blob (confirms client-side first).
    Cancel,
    /// Step 5 — download the finished archive, open its frames and save it
    /// locally (§ Download flow). Refused unless the session is `completed`
    /// and this app has the delivery seam wired.
    Download,
    /// Step 5 — immediate blob unlink + row delete (before the 30-day GC).
    Discard,
}

/// One mailbox the scope step offers, as `fauna.bridges.list_own_mailboxes`
/// reports it. Not a snapshot type — the per-mailbox counts feed the
/// `total_count` estimate and the UID-validity the EML manifest records, and
/// neither is rendered.
///
/// ⚠ **The listing order is the export order.** The nest returns these
/// ascending by the name's **raw bytes**, which is the first component of
/// § Container shape's total order; [`fauna_mail::export::ExportSerializer`]
/// refuses a run that arrives in any other order. A caller that re-sorts for
/// display must still walk *this* order when exporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportMailboxCount {
    pub name: String,
    /// Live message count — the wizard's per-mailbox "n messages" and the
    /// `total_count` estimate's only source.
    pub exists: u32,
    /// The mailbox's IMAP `UIDVALIDITY`, recorded per message in the EML-zip
    /// manifest.
    pub uid_validity: u32,
}

/// One message on the down-leg, as the seam hands it up.
///
/// `sealed_body` is **already materialized**: an over-frame record crosses as a
/// `body_ref` on the bulk-byte plane, and resolving that reference is platform
/// transport (native vs wasm fetcher), so the `rpc_glue` impl does it and this
/// type never carries the reference. That is what keeps [`run_export`] free of
/// any platform branch.
///
/// [`run_export`]: MailExportMachine::run_export
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRecord {
    pub mailbox: String,
    pub uid: u32,
    /// Canonical IMAP flag tokens.
    pub flags: Vec<String>,
    /// INTERNALDATE, epoch seconds — the entry mtime and the date the scope's
    /// range admits by.
    pub internal_date: i64,
    /// The record's **seal instant** (unix seconds; the nest's `stored_at`) —
    /// what the record opener picks epoch keys by (`record_seal_instant`). `0`
    /// = unknown, and the opener falls back to [`Self::internal_date`].
    pub stored_at: i64,
    /// The sealed record bytes, exactly as they rest: a **bare inner**
    /// envelope, opened with
    /// [`fauna_mail::open_sealed_inner_record_with_keys`].
    pub sealed_body: Vec<u8>,
}

/// One page of the down-leg (`fauna.bridges.fetch_export_chunk_ciphertext`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportFetchPage {
    pub records: Vec<ExportRecord>,
    /// The cursor to pass next. Equal to the request's when the page was empty.
    pub next_after_uid: u32,
    /// ⚠ The **only** end-of-mailbox signal. A UID gap yields a short page in
    /// the middle of a mailbox, so inferring the end from an empty or short
    /// page truncates the archive silently.
    pub mailbox_done: bool,
}

/// The counters one `upload_export_chunk` folds into the session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportChunkProgress {
    pub exported_delta: u64,
    /// The client's own resume marker after this frame.
    pub last_processed_message_id: String,
    /// Revised estimate once enumeration has finished, `None` while it has not.
    pub revised_total_count: Option<u64>,
}

/// What the nest answers an accepted chunk with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportUploadAck {
    pub blob_bytes: u64,
    /// `chunk_idx + 1`. The client seals from its own sealer's counter, so this
    /// is a cross-check, not the source of the next index.
    pub next_chunk_idx: u64,
    pub exported_count: u64,
}

/// What a generation-carrying seam call can answer beyond the seam's two
/// classes (`mail-export.md` § Resume). Typed rather than folded into
/// [`NestError::Rejected`]'s string because the machine's reaction to each is
/// unlike its reaction to any other refusal — and a reaction keyed on message
/// text is one rewording away from cancelling another device's export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportSeamError {
    /// `fauna.bridges.export_stream_superseded`: another of the user's devices
    /// restarted this export, and the generation this run drives is no longer
    /// the session's. The run drops itself and touches nothing — above all not
    /// the failure path, whose cancel would dispose of the stream that replaced
    /// its own.
    Superseded,
    Nest(NestError),
}

impl From<NestError> for ExportSeamError {
    fn from(e: NestError) -> Self {
        Self::Nest(e)
    }
}

impl std::fmt::Display for ExportSeamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Superseded => f.write_str(SUPERSEDED_MESSAGE),
            Self::Nest(e) => e.fmt(f),
        }
    }
}

impl From<ExportSeamError> for DispatchError {
    fn from(e: ExportSeamError) -> Self {
        match e {
            ExportSeamError::Nest(e) => Self::Nest(e),
            other => Self::InvalidState(other.to_string()),
        }
    }
}

/// Shown when this app's run finds another device has restarted the export.
const SUPERSEDED_MESSAGE: &str =
    "this export was restarted from another app session and continues there";

/// Shown when Resume is pressed on an export this app is not driving and
/// cannot faithfully re-run (its scope is unreadable here) — cancel and rerun.
const COLD_RESUME_UNSUPPORTED_MESSAGE: &str = "this export was started by another app session, whose in-progress archive cannot be \
     continued here — cancel it and run the wizard again";

/// WS-RPC seam to nest — the twelve User-class kinds of `mail-export.md`
/// § Wire shapes, one method each. Dual `async_trait` arm + [`MaybeSendSync`]
/// supertrait so the one seam serves native + wasm (mirrors `ForwarderNest`).
///
/// Session ids are **nest-minted UUID strings**, not bytes: the nest mints
/// `uuid::Uuid::new_v4().to_string()` and every wire type carries
/// `session_id: String`, which is the import twin's shape that
/// `mailbox-migration.md` makes this doc's precedent.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait MailExportNest: MaybeSendSync {
    /// `fauna.bridges.list_own_mailboxes` — the scope step's options, ascending
    /// by the name's raw bytes. A **User-class, caller-scoped** kind, distinct
    /// from the `BridgeMda`-only `fauna.bridges.list_mailboxes` (which takes a
    /// target actor): a User kind must offer no way to name another user's
    /// mailboxes (§ Cross-actor isolation).
    async fn list_own_mailboxes(&self) -> Result<Vec<ExportMailboxCount>, NestError>;
    /// `fauna.bridges.list_export_sessions` — the active/recent sessions
    /// (resume on client restart); the machine renders the newest non-terminal
    /// one.
    async fn list_export_sessions(&self) -> Result<Vec<ExportSessionView>, NestError>;
    /// `fauna.bridges.start_export_session` — the wizard's durable commit.
    /// `wrapped_session_key` is § Key material's per-session key, minted and
    /// wrapped client-side; the nest **requires** it (a session without one has
    /// produced a blob no client can ever open).
    async fn start_export_session(
        &self,
        format: ExportFormat,
        scope: ExportScope,
        wrapped_session_key: Vec<u8>,
        total_count: u64,
    ) -> Result<ExportSessionView, NestError>;
    /// `fauna.bridges.fetch_export_chunk_ciphertext` — the down-leg. Pages one
    /// mailbox by ascending UID; `after_uid` is exclusive, 0 starts it.
    async fn fetch_export_chunk_ciphertext(
        &self,
        session_id: String,
        mailbox: String,
        after_uid: u32,
    ) -> Result<ExportFetchPage, NestError>;
    /// `fauna.bridges.upload_export_chunk` — the up-leg. `sealed_chunk` is
    /// already framed and sealed; the nest appends it verbatim and parses
    /// nothing. `chunk_idx` must be the session's `next_chunk_idx` — take it
    /// from the sealer, never from a counter of your own. `stream_generation`
    /// is the generation the run was opened under (§ Resume).
    async fn upload_export_chunk(
        &self,
        session_id: String,
        stream_generation: u64,
        chunk_idx: u64,
        sealed_chunk: Vec<u8>,
        progress: ExportChunkProgress,
    ) -> Result<ExportUploadAck, ExportSeamError>;
    /// `fauna.bridges.pause_export_session`.
    ///
    /// `as_driver_of` — here and on resume / cancel / finalize — is § Resume's
    /// only-while-I-am-still-the-driver condition: `Some(generation)` from a
    /// run acting on its own session, `None` for the user's own control.
    async fn pause_export_session(
        &self,
        session_id: String,
        as_driver_of: Option<u64>,
    ) -> Result<ExportSessionView, ExportSeamError>;
    /// `fauna.bridges.resume_export_session` — the **warm** resume.
    async fn resume_export_session(
        &self,
        session_id: String,
        as_driver_of: Option<u64>,
    ) -> Result<ExportSessionView, ExportSeamError>;
    /// `fauna.bridges.restart_export_session` — the **cold** resume: a new
    /// stream generation on the same session, sealed under the fresh
    /// `wrapped_session_key`. The returned view carries the new generation.
    async fn restart_export_session(
        &self,
        session_id: String,
        wrapped_session_key: Vec<u8>,
        total_count: u64,
    ) -> Result<ExportSessionView, ExportSeamError>;
    /// `fauna.bridges.cancel_export_session` — abort + unlink the partial blob.
    async fn cancel_export_session(
        &self,
        session_id: String,
        as_driver_of: Option<u64>,
    ) -> Result<(), ExportSeamError>;
    /// `fauna.bridges.finalize_export_session` — the terminator frame is
    /// uploaded; close the session and mint the download URL.
    async fn finalize_export_session(
        &self,
        session_id: String,
        as_driver_of: Option<u64>,
    ) -> Result<ExportSessionView, ExportSeamError>;
    /// `fauna.bridges.fail_export_session` — record a condition fatal to the
    /// whole export: the row goes `errored` carrying `reason`, the partial blob
    /// is unlinked and the concurrency slot is freed (`mail-export.md`
    /// § Resume). The same disposal the failure path used to reach through
    /// `cancel_export_session`, plus the record of why — which is what lets the
    /// user's *other* devices, and this one after a restart, see that the
    /// export failed rather than that somebody cancelled it.
    ///
    /// `as_driver_of` carries the same condition as the transitions above and
    /// is always `Some` here, because only a driver ever fails a session.
    async fn fail_export_session(
        &self,
        session_id: String,
        reason: String,
        as_driver_of: Option<u64>,
    ) -> Result<(), ExportSeamError>;
    /// `fauna.bridges.discard_export_blob` — immediate unlink + row delete.
    async fn discard_export_blob(&self, session_id: String) -> Result<(), NestError>;
}

/// A freshly minted per-export-session key and the blob the nest stores
/// (`mail-export.md` § Key material).
pub struct MintedExportSessionKey {
    /// The raw key. Held `Zeroizing` and never leaves shared Rust: the machine
    /// hands it straight to [`fauna_mail::export::seal::ExportBlobSealer`] and
    /// drops it when the run ends.
    pub key: zeroize::Zeroizing<[u8; 32]>,
    /// The `ExportSessionKeyBlob`'s canonical bytes — what
    /// `start_export_session` carries and `export_sessions` stores.
    pub wrapped: Vec<u8>,
}

/// Opens one fetched record under the account's mail read keys.
///
/// An object rather than a method on the custody seam because the key set is
/// derived **once per run** and then used per message: an async re-derivation
/// per record would re-load the mail custody for every message in the mailbox. The
/// keys stay inside this object in shared Rust — nothing hands them to app
/// glue, which is the same custody rule `MailKeyCache` keeps for the receive
/// path (`conversations.md` § Architectural rules #2).
pub trait MailRecordOpening: MaybeSendSync {
    /// `seal_instant` is the record's seal instant ([`record_seal_instant`]),
    /// used only to pick the epoch keys to trial.
    fn open(&self, sealed_body: &[u8], seal_instant: u64) -> Result<Vec<u8>, DispatchError>;
}

/// The account's mail key material, as the export loop needs it
/// (`mail-export.md` § Key material). Implemented by `MailSettingsMachine`,
/// which already holds the `MailStore` the MSEK lives in — the same
/// composition `build_mail_spam_machine` uses for `SealedModelWriter`.
///
/// Both methods answer `Err` when mail is not enabled for the actor: there is
/// no MSEK, so there is neither a key to wrap the session key to nor a key set
/// to open records with, and an export of a mailbox that does not exist is not
/// a degraded case to paper over.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait MailExportKeyCustody: MaybeSendSync {
    /// Mint a fresh 256-bit session key and wrap it to the account's own
    /// current-generation recipient key.
    async fn mint_export_session_key(&self) -> Result<MintedExportSessionKey, DispatchError>;
    /// Derive the run's record opener: the complete standing key set (current +
    /// grace) plus the mail-epoch roots.
    async fn export_record_opener(&self) -> Result<Arc<dyn MailRecordOpening>, DispatchError>;
    /// Unwrap a session's stored `wrapped_session_key` — § Download flow step 4.
    ///
    /// The mint half's inverse, and deliberately a *separate* call rather than
    /// a key the run kept: the client that downloads is not necessarily the one
    /// that exported (a second device, or the same one after a restart), so the
    /// only durable copy is the row's, and it opens under the account's
    /// **complete** standing set — current plus grace — so a client that has
    /// rotated since the export still opens the archive it produced.
    async fn unwrap_export_session_key(
        &self,
        wrapped: &[u8],
    ) -> Result<zeroize::Zeroizing<[u8; 32]>, DispatchError>;
}

/// The platform half of § Download flow: the authenticated streaming GET of
/// the nest's per-session blob route, and the local file the user ends up with.
///
/// **Why a seam at all**, when the opening is pure shared Rust: both ends are
/// genuinely per-platform. The GET rides the session's own pinned HTTP client
/// and bearer cache, which only the native/wasm client crates hold; and "where
/// a saved file goes" is a downloads directory on a desktop and a save dialog
/// on the web. Everything between — unwrapping the key, opening the frames in
/// order, refusing an over-long frame or a missing terminator — is in
/// [`MailExportMachine::download`] and shared by every app (priority #2).
///
/// **Streaming on both halves, and not as a nicety.** § Quota composition caps
/// a blob at 10 GiB, so neither the sealed body nor the recovered archive may
/// be buffered whole: the machine pulls one slice at a time, opens whatever
/// frames complete, and writes their plaintext straight out. Peak memory is one
/// frame — the bound [`fauna_mail::export::MAX_EXPORT_FRAME_BYTES`] makes true
/// against a hostile nest.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait ExportArchiveDelivery: MaybeSendSync {
    /// Begin the actor-authed `GET download_url` against the session's own
    /// nest. `download_url` is the nest-minted path from the session row —
    /// never one the client composes.
    async fn open_download(
        &self,
        download_url: String,
    ) -> Result<Box<dyn SealedBlobStream>, DispatchError>;
    /// Create the local destination named `file_name` (§ Compression wrapper's
    /// `fauna-export-<handle>-<format>-<iso-date>.zip.zst`).
    async fn create_archive(
        &self,
        file_name: String,
    ) -> Result<Box<dyn ArchiveFileSink>, DispatchError>;
}

/// The sealed blob arriving from the nest, in order, a slice at a time.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait SealedBlobStream: MaybeSendSync {
    /// The next slice of the body, or `None` at its end. Slice boundaries carry
    /// no meaning — the opener reassembles frames across them.
    async fn next_slice(&mut self) -> Result<Option<Vec<u8>>, DispatchError>;
}

/// The local file the recovered archive is written into.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait ArchiveFileSink: MaybeSendSync {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), DispatchError>;
    /// Close the destination and answer the location to show the user.
    ///
    /// Takes `Box<Self>` so an implementation can consume its handle: a sink
    /// that could be written to after `finish` would let a caller append past
    /// the terminator the opener just verified.
    async fn finish(self: Box<Self>) -> Result<String, DispatchError>;
}

/// The production [`MailRecordOpening`]: the account's complete standing key
/// set plus its mail-epoch roots, trialled through the one shared bare-inner
/// opener ([`fauna_mail::open_sealed_inner_record_with_keys`]) — the same set
/// and the same trial order the receive path opens mail with, so a record the
/// user's inbox can read is by construction one the export can read.
pub struct StandingKeyRecordOpener {
    standing: Vec<fauna_mls::wrapped_blob::StandingMailKeypair>,
    /// Newest generation first, aligned with `standing`.
    epoch_roots: Vec<zeroize::Zeroizing<[u8; 32]>>,
    /// Each prior generation's retirement instant, aligned with `standing[1..]`
    /// — the seal-time trial order.
    retired_at_unix: Vec<u64>,
}

impl StandingKeyRecordOpener {
    /// Derive the opener from the account's MSEK history — the current MSEK
    /// first, then every retired generation (uncapped) — with each prior's
    /// retirement instant (`retired_at_unix`, aligned with `mseks[1..]`).
    pub fn from_msek_history(mseks: &[[u8; 32]], retired_at_unix: &[u64]) -> Self {
        Self {
            standing: fauna_mls::wrapped_blob::derive_standing_mail_keypairs(mseks),
            epoch_roots: mseks
                .iter()
                .map(fauna_mls::wrapped_blob::derive_mail_epoch_root)
                .collect(),
            retired_at_unix: retired_at_unix.to_vec(),
        }
    }
}

impl MailRecordOpening for StandingKeyRecordOpener {
    fn open(&self, sealed_body: &[u8], seal_instant: u64) -> Result<Vec<u8>, DispatchError> {
        let roots: Vec<&[u8; 32]> = self.epoch_roots.iter().map(|r| &**r).collect();
        fauna_mail::open_sealed_inner_record_with_keys(
            sealed_body,
            &roots,
            seal_instant,
            &self.standing,
            &self.retired_at_unix,
        )
        .map_err(|e| DispatchError::InvalidState(format!("open record: {e}")))
    }
}

/// Mint a fresh per-session key and wrap it to the account's own
/// current-generation X-Wing key (`mail-export.md` § Key material). Shared so
/// `MailSettingsMachine` and every test mint through the one path.
pub fn mint_export_session_key_for(
    msek: &[u8; 32],
    actor_id: &[u8; 32],
) -> Result<MintedExportSessionKey, DispatchError> {
    use rand::RngCore;
    let mut key = zeroize::Zeroizing::new([0u8; 32]);
    rand::thread_rng().fill_bytes(key.as_mut_slice());
    let recipient = fauna_mls::wrapped_blob::derive_recipient_xwing_keypair(msek).public;
    let wrapped = fauna_mls::wrapped_blob::seal_export_session_key(&key, actor_id, &recipient)
        .and_then(|blob| blob.to_canonical_bytes())
        .map_err(|e| DispatchError::InvalidState(format!("wrap export session key: {e}")))?;
    Ok(MintedExportSessionKey { key, wrapped })
}

/// § Compression wrapper's download name:
/// `fauna-export-<actor-handle>-<format>-<iso-date>.zip.zst` — self-documenting,
/// sortable, and deliberately free of the session id, which § Architectural
/// rules calls sensitive and which would otherwise ride in a filename the user
/// may well mail to themselves.
///
/// The date is the day the archive was **downloaded**, read from the clock at
/// this one call site. That is not in tension with § Container shape's
/// determinism contract, which is a claim about the archive's *bytes*: the name
/// is chosen by the saving client, and two clients saving one blob on different
/// days should say so. The handle is sanitized the same way the archive's root
/// directory is, so a handle that is not a safe path component cannot make an
/// unsaveable name.
fn archive_file_name(actor_handle: &str, format: ExportFormat, now_secs: i64) -> String {
    let (y, m, d) = fauna_core::caltime::civil_from_days(now_secs.div_euclid(86_400));
    // The empty case is checked BEFORE encoding: `encode_path_component("")`
    // answers `"%"` — never empty, by design, because an empty archive path
    // component is unrepresentable — so an is-empty test after it never fires
    // and the user gets `fauna-export-%-mbox-…`. Only the seam-only build has
    // no handle, and it cannot reach this call, so this is a belt.
    let encoded;
    let handle = if actor_handle.is_empty() {
        "export"
    } else {
        encoded = fauna_mail::export::encode_path_component(actor_handle);
        &encoded
    };
    format!(
        "fauna-export-{handle}-{}-{y:04}-{m:02}-{d:02}.zip.zst",
        format.wire_name()
    )
}

/// The instant the record opener picks epoch keys by: the record's **seal
/// instant**, never its own date.
///
/// The receive path's rule (`fauna_mail::open_inbound_record_epoch_hybrid`:
/// "NEVER a sender-supplied `Date:` header"), and for the same reason. A record
/// is sealed to the mail epoch it was *stored* in; INTERNALDATE is the message's
/// date, which for imported mail is years earlier and for ordinary mail can
/// already sit in the previous weekly epoch when it arrives. The epoch chain
/// trials its target, the epoch before, the standing keys and then only
/// *earlier* epochs — so a target taken from INTERNALDATE misses every record
/// sealed after the epoch its date names, and the standing arm cannot catch an
/// epoch-sealed record. On an egress path that is an export that fails
/// (§ An unopenable record fails the session). INTERNALDATE is the fallback only
/// when the nest reported no seal instant.
fn record_seal_instant(record: &ExportRecord) -> u64 {
    let instant = if record.stored_at > 0 {
        record.stored_at
    } else {
        record.internal_date
    };
    u64::try_from(instant).unwrap_or(0)
}

/// One instance per user client. Holds the rendered snapshot; drives the seam.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MailExportMachine {
    nest: Arc<dyn MailExportNest>,
    /// The account's mail key material (§ Key material). `None` only in the
    /// unit tests' `FakeNest` wiring, where no MSEK exists to derive from;
    /// every real `build_mail_export_machine` supplies it, and `Start` refuses
    /// without it rather than opening a session whose blob nothing can open.
    keys: Option<Arc<dyn MailExportKeyCustody>>,
    /// The platform's download + local-save half (§ Download flow). `None` for
    /// an app whose glue does not wire it yet, and for the seam-only build —
    /// [`MailExportAction::Download`] then refuses honestly rather than
    /// claiming to have saved something.
    delivery: Option<Arc<dyn ExportArchiveDelivery>>,
    /// The user's handle, which names the archive's single root directory
    /// (`<handle>-mbox` etc., § Format choices) and the saved file. Session
    /// state the app glue holds — it is not in the account plane and not derivable
    /// from the actor id — and **live, not baked**: an app may learn the handle
    /// after it built this machine (a fresh sign-in fetches it asynchronously)
    /// or the user may change it, so the glue refreshes it through
    /// [`MailExportMachine::set_actor_handle`] and each run and each download
    /// reads it once, at its start.
    actor_handle: Mutex<String>,
    inner: Mutex<MailExportSnapshot>,
    /// The active session as the nest last reported it. Kept out of the public
    /// snapshot — the session_id is sensitive and the UI never displays it
    /// (`mail-export.md` § Architectural rules) — but the machine needs the id
    /// to drive the pipeline and the pause/resume/cancel/discard controls, and
    /// the format and scope to rebuild a run on a cold resume (§ Resume).
    session: Mutex<Option<ExportSessionView>>,
    /// Pause/Cancel requested while [`Self::run_export`] is mid-flight. Read at
    /// every page boundary, exactly as `run_import`'s does.
    stop: Mutex<StopRequest>,
    /// True while [`Self::run_export`] holds the run. While it does, the LOOP
    /// owns the session's pause transition: the nest refuses
    /// `upload_export_chunk` on any session that is not `running`, so a pause
    /// sent to the nest while a page is half-uploaded would make the next
    /// upload fail and — under § An unopenable record fails the session's "no
    /// silent loss" rule — cost the user the whole export. `Pause` therefore
    /// only asks, and the loop pauses the session itself at the page boundary.
    driving: std::sync::atomic::AtomicBool,
    /// The last `list_own_mailboxes` reply, kept whole. The rendered
    /// [`MailboxOption`] list carries only name + selection, but `Start` needs
    /// each mailbox's message count (the `total_count` estimate), its
    /// UID-validity (the EML manifest), and above all the **listing order**,
    /// which is § Container shape's total order and the order the run must
    /// walk in.
    catalogue: Mutex<Vec<ExportMailboxCount>>,
    /// The in-flight run's stream state — the serializer, the single zstd
    /// stream, and the frame sealer.
    ///
    /// ⚠ **This is why a paused export can only be *continued* by the same
    /// live machine.** § Container shape pins the blob as byte slices of *one*
    /// zstd stream, and a zstd stream cannot be picked up from the middle: a
    /// second process holds no encoder state. A warm pause/resume keeps this
    /// alive and continues it; a session found at hydrate time after the app
    /// restarted (or on the user's other device) has no run here, so its
    /// Resume is the **cold** one — `restart_export_session` opens a new stream
    /// generation on the same session and a fresh run is built here from the
    /// row's scope (`mail-export.md` § Resume).
    ///
    /// The lock is taken only to move the value in or out — never held across
    /// an `.await`.
    run: Mutex<Option<ExportRun>>,
    /// The up-leg chunk size — [`fauna_mail::export::EXPORT_CHUNK_BYTES`] in
    /// every real build. A field only so the unit tests can drive a run across
    /// many frames without 16 MiB of fixture mail.
    chunk_bytes: usize,
}

/// Pause/Cancel arriving while the drive loop runs (the export twin of
/// `import.rs`'s own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopRequest {
    None,
    Pause,
    Cancel,
}

/// The stream state one export run carries between pages. Lives in
/// [`MailExportMachine::run`] so a Pause can put it back and a Resume can pick
/// it up; `run_export` holds it locally while driving, so no lock is held
/// across an await.
struct ExportRun {
    session_id: String,
    /// The stream generation this run was opened under — 0 from `Start`, the
    /// restart reply's from a cold resume. Rides every call the run makes.
    generation: u64,
    /// Per-format serializer — enforces § Container shape's total order and
    /// refuses a run that arrives out of it.
    serializer: fauna_mail::export::ExportSerializer,
    /// The one zip-inside-one-zstd stream whose byte slices are the chunks.
    stream: fauna_mail::export::ExportArchiveStream,
    /// Frames and seals each slice under the per-session key (§ Blob shape on
    /// disk). Owns the chunk index the nest validates against.
    sealer: fauna_mail::export::ExportBlobSealer,
    opener: Arc<dyn MailRecordOpening>,
    /// The selected mailboxes still to walk, in `list_own_mailboxes` order, and
    /// each one's exclusive UID cursor.
    remaining: Vec<ExportMailboxCount>,
    after_uid: u32,
    exported: u64,
    /// Messages exported from the mailbox currently being walked — in the run,
    /// not a loop local, so a pause mid-mailbox does not reset its progress row.
    exported_here: u32,
    /// The resume marker the nest stores — the last record's `mailbox:uid`.
    last_processed: String,
    /// The scope's date range (§ UX shape step 2). A record outside it is
    /// outside the export's scope — never fetched into the archive, never
    /// opened, and never counted as *skipped*: `skipped_count` is for messages
    /// the export wanted and could not take.
    range: ExportDateRange,
    /// A tightened `total_count` estimate waiting for the next upload to carry
    /// it to the nest. The listing's per-mailbox counts are whole-mailbox, so
    /// under a range the estimate only becomes true as mailboxes finish.
    revised_total: Option<u64>,
}

impl MailExportMachine {
    /// The full wiring every real app uses.
    pub fn new(
        nest: Arc<dyn MailExportNest>,
        keys: Arc<dyn MailExportKeyCustody>,
        delivery: Arc<dyn ExportArchiveDelivery>,
        actor_handle: impl Into<String>,
    ) -> Self {
        Self::with_optional_keys(nest, Some(keys), Some(delivery), actor_handle)
    }

    /// Custody but no download half: the app drives a real export and saves
    /// nothing, so [`MailExportAction::Download`] refuses honestly.
    ///
    /// The shape for an app whose glue has no save destination yet (web's,
    /// until its trickle-down passed in a browser delivery). Unlike the
    /// custody-less build this is NOT a fake green: the export really runs, the
    /// archive really rests on the nest for § Expiry's 30 days, and only the
    /// last step is missing and says so.
    pub fn without_archive_delivery(
        nest: Arc<dyn MailExportNest>,
        keys: Arc<dyn MailExportKeyCustody>,
        actor_handle: impl Into<String>,
    ) -> Self {
        Self::with_optional_keys(nest, Some(keys), None, actor_handle)
    }

    /// Seam-only wiring: every control but `Start` works over the real seam,
    /// and `Start` refuses before any session exists. The build for an app
    /// whose glue does not yet spawn [`Self::run_export`] — giving such an app
    /// custody would let `Start` open a session nothing ever drives, a fake
    /// green (`rpc_glue::build_mail_export_machine_without_key_custody`).
    pub fn without_key_custody(nest: Arc<dyn MailExportNest>) -> Self {
        Self::with_optional_keys(nest, None, None, "")
    }

    fn with_optional_keys(
        nest: Arc<dyn MailExportNest>,
        keys: Option<Arc<dyn MailExportKeyCustody>>,
        delivery: Option<Arc<dyn ExportArchiveDelivery>>,
        actor_handle: impl Into<String>,
    ) -> Self {
        Self {
            nest,
            keys,
            delivery,
            actor_handle: Mutex::new(actor_handle.into()),
            inner: Mutex::new(MailExportSnapshot::empty()),
            session: Mutex::new(None),
            stop: Mutex::new(StopRequest::None),
            driving: std::sync::atomic::AtomicBool::new(false),
            catalogue: Mutex::new(Vec::new()),
            run: Mutex::new(None),
            chunk_bytes: fauna_mail::export::EXPORT_CHUNK_BYTES,
        }
    }

    #[cfg(test)]
    fn with_chunk_bytes(mut self, chunk_bytes: usize) -> Self {
        self.chunk_bytes = chunk_bytes;
        self
    }

    fn set_status(&self, status: ExportStatus) {
        self.inner.lock().expect("snapshot mutex").status = status;
    }

    /// Record a session: stash its id (for later controls) + project it into the
    /// snapshot's progress/done fields.
    fn store_session(&self, session: ExportSessionView) {
        *self.session.lock().expect("session mutex") = Some(session.clone());
        self.inner
            .lock()
            .expect("snapshot mutex")
            .apply_session(session);
    }

    /// The id of the currently-rendered session, if any.
    fn current_session_id(&self) -> Result<String, DispatchError> {
        self.session
            .lock()
            .expect("session mutex")
            .as_ref()
            .map(|s| s.session_id.clone())
            .ok_or_else(|| DispatchError::InvalidState("no active export session".into()))
    }

    /// Clear the active session (cancel / discard) + return the wizard to start.
    fn clear_session(&self) {
        *self.session.lock().expect("session mutex") = None;
        // `stop` is deliberately left as it is: a Cancel must still read as
        // Cancel to a loop that is finishing its page, or it carries on into
        // a session the nest has already disposed of. `start()` clears it.
        // Dropping the run drops the session key with it.
        *self.run.lock().expect("run mutex") = None;
        self.inner.lock().expect("snapshot mutex").reset_to_format();
    }

    /// Page load / resume. Loads the mailbox options and, if an active session
    /// exists, jumps to its Progress/Done screen. The wizard stays usable (the
    /// Format step) even when the seam reads fail — the error is surfaced but
    /// the client-side steps still navigate.
    async fn refresh(&self) -> Result<(), DispatchError> {
        self.set_status(ExportStatus::Loading);
        let mailboxes = self.nest.list_own_mailboxes().await?;
        let sessions = self.nest.list_export_sessions().await?;
        // Resume the newest non-terminal session, if any (running / paused /
        // completed-not-yet-discarded).
        let active = sessions.into_iter().find(|s| {
            !matches!(
                s.state,
                ExportSessionState::Cancelled | ExportSessionState::Errored
            )
        });
        {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            // Preserve any in-progress selection across a refresh; only (re)seed
            // the mailbox options when we don't have them yet.
            if snap.mailboxes.is_empty() {
                snap.mailboxes = mailboxes
                    .iter()
                    .map(|m| MailboxOption {
                        selected: default_selected(&m.name),
                        name: m.name.clone(),
                    })
                    .collect();
            }
            snap.status = ExportStatus::Idle;
        }
        // The counts and UID-validities ride beside the options: the scope
        // step's estimate needs the counts and the EML manifest needs the
        // validities, and neither is a rendered field.
        *self.catalogue.lock().expect("catalogue mutex") = mailboxes;
        if let Some(active) = active {
            self.store_session(active);
        } else if self.session.lock().expect("session mutex").is_some() {
            // The session this machine holds is no longer live on the nest —
            // discarded, cancelled or failed from another of the user's
            // devices, or reclaimed by § Expiry. Keeping it would paint a Done
            // screen whose download names a blob that no longer exists (or a
            // Progress screen nothing drives). Only a HELD session is dropped:
            // a wizard mid-way through Format/Scope with no session keeps its
            // choices across a refresh.
            self.clear_session();
        }
        Ok(())
    }

    /// The wizard's durable commit (§ UX shape step 3): mint and wrap the
    /// per-session key, open the `export_sessions` row, and build the run state
    /// the drive loop will use. The app glue calls [`Self::run_export`] right
    /// after a successful `Start`, exactly as it does for the import twin.
    async fn start(&self) -> Result<(), DispatchError> {
        self.set_status(ExportStatus::Working);
        let keys = self.key_custody()?;
        let (format, scope) = {
            let snap = self.inner.lock().expect("snapshot mutex");
            (snap.format, snap.scope())
        };
        // Walk the mailboxes in the nest's listing order (raw-byte ascending),
        // keeping only the selected ones — NOT in the order the scope step
        // happens to hold them. That order is § Container shape's, and the
        // serializer refuses a run that arrives in any other.
        let selected: Vec<ExportMailboxCount> = {
            let catalogue = self.catalogue.lock().expect("catalogue mutex");
            catalogue
                .iter()
                .filter(|m| scope.mailboxes.iter().any(|s| s == &m.name))
                .cloned()
                .collect()
        };
        let total_count: u64 = selected.iter().map(|m| u64::from(m.exists)).sum();
        // Resolved BEFORE the session opens: a malformed range must refuse the
        // Start, not surface after a concurrency slot is already taken.
        let range = ExportDateRange::from_scope(&scope)?;

        // Minted before the RPC, because the nest REQUIRES the wrapped key on
        // start: a session opened without one has produced a blob no client can
        // ever open (§ Key material).
        let minted = keys.mint_export_session_key().await?;
        let opener = keys.export_record_opener().await?;
        let strip_headers = scope.strip_headers;
        let session = self
            .nest
            .start_export_session(format, scope, minted.wrapped, total_count)
            .await?;
        self.open_run(
            &session,
            strip_headers,
            range,
            selected,
            &minted.key,
            opener,
        )?;
        self.store_session(session);
        self.set_status(ExportStatus::Idle);
        Ok(())
    }

    /// Build the stream state for `session`'s **current generation**, from its
    /// first message, and park it for [`Self::run_export`] — the tail `Start`
    /// and the cold resume share, because a restarted stream is a started one:
    /// same serializer, same single zstd stream, a sealer under the key just
    /// minted for this generation.
    fn open_run(
        &self,
        session: &ExportSessionView,
        strip_headers: bool,
        range: ExportDateRange,
        selected: Vec<ExportMailboxCount>,
        session_key: &[u8; 32],
        opener: Arc<dyn MailRecordOpening>,
    ) -> Result<(), DispatchError> {
        let sealer = fauna_mail::export::ExportBlobSealer::new(
            session_key,
            &session.session_id,
            session.format.wire_name(),
        )
        .map_err(|e| DispatchError::InvalidState(format!("open export blob sealer: {e}")))?;
        let stream = fauna_mail::export::ExportArchiveStream::new(self.chunk_bytes)
            .map_err(|e| DispatchError::InvalidState(format!("open export archive stream: {e}")))?;
        let mut options = fauna_mail::export::ExportOptions::new(self.actor_handle());
        options.strip_headers = strip_headers;
        {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            snap.mailbox_progress = selected
                .iter()
                .map(|m| MailboxProgressView {
                    name: m.name.clone(),
                    exported: 0,
                    total: m.exists,
                })
                .collect();
            snap.error_log.clear();
        }
        *self.stop.lock().expect("stop mutex") = StopRequest::None;
        *self.run.lock().expect("run mutex") = Some(ExportRun {
            session_id: session.session_id.clone(),
            generation: session.stream_generation,
            serializer: fauna_mail::export::ExportSerializer::new(session.format, options),
            stream,
            sealer,
            opener,
            remaining: selected,
            after_uid: 0,
            exported: 0,
            exported_here: 0,
            last_processed: String::new(),
            range,
            revised_total: None,
        });
        Ok(())
    }

    /// The generation of the run parked here for `session_id`, if there is one
    /// — i.e. whether this machine holds that session's live stream.
    fn parked_generation(&self, session_id: &str) -> Option<u64> {
        self.run
            .lock()
            .expect("run mutex")
            .as_ref()
            .filter(|r| r.session_id == session_id)
            .map(|r| r.generation)
    }

    async fn pause(&self) -> Result<(), DispatchError> {
        self.set_status(ExportStatus::Working);
        let id = self.current_session_id()?;
        *self.stop.lock().expect("stop mutex") = StopRequest::Pause;
        if self.driving.load(std::sync::atomic::Ordering::SeqCst) {
            // The loop finishes the page it holds — its chunks can only upload
            // while the session is still `running` — then pauses the session
            // itself and parks the run (see the `driving` field).
            return Ok(());
        }
        let parked = self.parked_generation(&id);
        let running = self.inner.lock().expect("snapshot mutex").session_state
            == Some(ExportSessionState::Running);
        if parked.is_none() && running {
            // A `running` export this machine holds no stream for is being
            // driven somewhere else — or by nobody. Pausing it helps in neither
            // case and is ruinous in the first: the nest refuses uploads to a
            // paused session, so the device that IS driving would fail its next
            // chunk and lose the whole export (§ Resume).
            *self.stop.lock().expect("stop mutex") = StopRequest::None;
            return Err(DispatchError::InvalidState(
                "this export is not running in this app session — pause it where it is \
                 running, or press Resume to restart it here"
                    .into(),
            ));
        }
        match self.nest.pause_export_session(id.clone(), parked).await {
            Ok(session) => self.store_session(session),
            Err(ExportSeamError::Superseded) => return self.superseded(&id).await,
            Err(e) => return Err(e.into()),
        }
        self.set_status(ExportStatus::Idle);
        Ok(())
    }

    /// Resume — **warm** when this machine holds the session's parked stream
    /// (continue it), **cold** otherwise (restart it here on a new stream
    /// generation). `mail-export.md` § Resume owns the split. Either way the
    /// app glue calls [`Self::run_export`] right after, as it does after
    /// `Start`.
    async fn resume(&self) -> Result<(), DispatchError> {
        self.set_status(ExportStatus::Working);
        let id = self.current_session_id()?;
        *self.stop.lock().expect("stop mutex") = StopRequest::None;
        if self.driving.load(std::sync::atomic::Ordering::SeqCst) {
            // The loop is still finishing the page a Pause asked it to stop
            // after; clearing the request above is the whole resume. (The run
            // is the loop's local right now, so the parked check below would
            // misread this as a cold session and restart our own export.)
            return Ok(());
        }
        match self.parked_generation(&id) {
            Some(generation) => {
                match self
                    .nest
                    .resume_export_session(id.clone(), Some(generation))
                    .await
                {
                    Ok(session) => self.store_session(session),
                    Err(ExportSeamError::Superseded) => return self.superseded(&id).await,
                    Err(e) => return Err(e.into()),
                }
            }
            None => self.restart_here(id).await?,
        }
        self.set_status(ExportStatus::Idle);
        Ok(())
    }

    /// The cold resume (§ Resume): this machine holds no stream for the
    /// session — the app that did has exited, or is the user's other device —
    /// so the export restarts here from the first message, on the same session.
    ///
    /// Everything the run needs is re-derived, never inherited from the
    /// abandoned stream: the format and scope from the row, the mailbox order
    /// and counts from a fresh listing, and a **fresh** session key (one key
    /// per generation — so this path never unwraps the old one).
    async fn restart_here(&self, session_id: String) -> Result<(), DispatchError> {
        let keys = self.key_custody()?;
        let view = self
            .session
            .lock()
            .expect("session mutex")
            .clone()
            .ok_or_else(|| DispatchError::InvalidState("no active export session".into()))?;
        // A row whose scope this client cannot read is one it cannot faithfully
        // re-run: exporting "everything" in its place would hand back a
        // different archive under the same session.
        let Some(scope) = view.scope.clone() else {
            return Err(DispatchError::InvalidState(
                COLD_RESUME_UNSUPPORTED_MESSAGE.into(),
            ));
        };
        // The row's range, exactly as its `strip_headers`: a restarted export
        // is the SAME export, and a range this client cannot read refuses the
        // restart rather than widening it to everything.
        let range = ExportDateRange::from_scope(&scope)?;
        // A fresh listing, not the hydrate-time catalogue: the counts feed the
        // restarted progress bar, and the mailboxes may have moved since.
        let catalogue = self.nest.list_own_mailboxes().await?;
        let selected: Vec<ExportMailboxCount> = catalogue
            .iter()
            .filter(|m| scope.mailboxes.iter().any(|s| s == &m.name))
            .cloned()
            .collect();
        *self.catalogue.lock().expect("catalogue mutex") = catalogue;
        let total_count: u64 = selected.iter().map(|m| u64::from(m.exists)).sum();

        let minted = keys.mint_export_session_key().await?;
        let opener = keys.export_record_opener().await?;
        let session = self
            .nest
            .restart_export_session(session_id, minted.wrapped, total_count)
            .await?;
        self.open_run(
            &session,
            scope.strip_headers,
            range,
            selected,
            &minted.key,
            opener,
        )?;
        self.store_session(session);
        Ok(())
    }

    /// This machine's run was restarted over from another device (§ Resume).
    /// Drop the stream and **touch nothing on the nest** — the session is alive
    /// and somebody else's now — then re-read it, so the Progress screen shows
    /// the restarted export as the cold session it has become here.
    async fn superseded(&self, session_id: &str) -> Result<(), DispatchError> {
        {
            let mut run = self.run.lock().expect("run mutex");
            if run.as_ref().is_some_and(|r| r.session_id == session_id) {
                *run = None;
            }
        }
        let still_current = self
            .session
            .lock()
            .expect("session mutex")
            .as_ref()
            .is_some_and(|s| s.session_id == session_id);
        if still_current {
            if let Ok(sessions) = self.nest.list_export_sessions().await
                && let Some(current) = sessions.into_iter().find(|s| s.session_id == session_id)
            {
                self.store_session(current);
            }
            // The per-mailbox rows were this run's; the other device's progress
            // arrives as aggregate counters only.
            self.inner
                .lock()
                .expect("snapshot mutex")
                .mailbox_progress
                .clear();
        }
        self.set_status(ExportStatus::Idle);
        Err(DispatchError::InvalidState(SUPERSEDED_MESSAGE.into()))
    }

    async fn cancel(&self) -> Result<(), DispatchError> {
        self.set_status(ExportStatus::Working);
        let id = self.current_session_id()?;
        *self.stop.lock().expect("stop mutex") = StopRequest::Cancel;
        // The user's own control: unconditional, whoever is driving (§ Resume).
        self.nest.cancel_export_session(id, None).await?;
        // Cancel returns the wizard to the start so the user can re-run it.
        self.clear_session();
        Ok(())
    }

    async fn discard(&self) -> Result<(), DispatchError> {
        self.set_status(ExportStatus::Working);
        let id = self.current_session_id()?;
        self.nest.discard_export_blob(id).await?;
        self.clear_session();
        Ok(())
    }

    /// § Download flow, end to end and streaming: GET the nest-minted URL,
    /// unwrap the row's session key, open the frames in order, write their
    /// plaintext out, and refuse an archive that never terminated.
    ///
    /// **What is saved is the recovered `.zip.zst`, verbatim — the client does
    /// not decompress.** § Container shape made the artifact one zip inside one
    /// zstd stream and ratified that the user runs `zstd -d` once to hold a
    /// plain `.zip` their file manager opens; step 5 says so in as many words.
    /// So the frames' concatenated plaintext IS the file, and nothing here has
    /// to hold a decoder, pick a place to expand a tree into, or decide what
    /// happens when part of that tree collides with something already there.
    ///
    /// **Nothing is buffered whole.** § Quota composition caps the blob at
    /// 10 GiB: slices are pulled one at a time, every frame that completes is
    /// written out immediately, and peak memory is one frame — which is only a
    /// bound because [`fauna_mail::export::MAX_EXPORT_FRAME_BYTES`] makes the
    /// declared length untrustable-but-bounded.
    ///
    /// **The refusal order is load-bearing.** `finish()` — the terminator check
    /// — runs before the sink is closed, so a truncated or still-running
    /// download never becomes a file on the user's disk that looks like their
    /// mailbox. § Architectural rules' no-partial-blob-download rule is exactly
    /// this call site.
    async fn download(&self) -> Result<(), DispatchError> {
        self.set_status(ExportStatus::Working);
        let session = self
            .session
            .lock()
            .expect("session mutex")
            .clone()
            .ok_or_else(|| DispatchError::InvalidState("no active export session".into()))?;
        if session.state != ExportSessionState::Completed {
            return Err(DispatchError::InvalidState(
                "this export has not finished yet — there is nothing to download".into(),
            ));
        }
        if session.download_url.is_empty() {
            return Err(DispatchError::InvalidState(
                "this export has no download link yet".into(),
            ));
        }
        if session.wrapped_session_key.is_empty() {
            // The nest refuses to open a session without one, so a completed
            // row that has none is a blob nobody can ever read — say that
            // rather than fail later inside the AEAD, which cannot tell a
            // missing key from a wrong one.
            return Err(DispatchError::InvalidState(
                "this export was made without a stored key, so it cannot be opened".into(),
            ));
        }
        let keys = self.key_custody()?;
        let delivery = self.delivery()?;

        let key = keys
            .unwrap_export_session_key(&session.wrapped_session_key)
            .await?;
        let mut opener = fauna_mail::export::ExportBlobOpener::new(
            key.as_slice(),
            &session.session_id,
            session.format.wire_name(),
        )
        .map_err(|e| DispatchError::InvalidState(format!("open export archive: {e}")))?;

        let mut stream = delivery.open_download(session.download_url.clone()).await?;
        let mut sink = delivery
            .create_archive(archive_file_name(
                &self.actor_handle(),
                session.format,
                fauna_core::data::Timestamp::now_secs(),
            ))
            .await?;
        while let Some(slice) = stream.next_slice().await? {
            for plaintext in opener
                .push(&slice)
                .map_err(|e| DispatchError::InvalidState(format!("open export archive: {e}")))?
            {
                sink.write(&plaintext).await?;
            }
        }
        // Before `finish()` on the sink: a blob without its terminator must not
        // leave a file behind that reads as a complete mailbox.
        opener
            .finish()
            .map_err(|e| DispatchError::InvalidState(format!("open export archive: {e}")))?;
        let saved = sink.finish().await?;
        self.inner
            .lock()
            .expect("snapshot mutex")
            .saved_archive_path = saved;
        self.set_status(ExportStatus::Idle);
        Ok(())
    }

    fn actor_handle(&self) -> String {
        self.actor_handle.lock().expect("handle mutex").clone()
    }

    fn delivery(&self) -> Result<Arc<dyn ExportArchiveDelivery>, DispatchError> {
        // The `key_custody` shape: an app whose glue has no download half yet
        // says so to the user rather than silently saving nothing.
        self.delivery.clone().ok_or_else(|| {
            DispatchError::InvalidState(
                "unimplemented: downloading an export is not yet available in this app".into(),
            )
        })
    }

    fn key_custody(&self) -> Result<Arc<dyn MailExportKeyCustody>, DispatchError> {
        // Shown on `error-message` in an app whose glue has not yet been wired
        // to drive the export (see `without_key_custody`), so it speaks to the
        // user, not the developer — and it is the same honest-rejection shape
        // the page showed while the backend was unbuilt.
        self.keys.clone().ok_or_else(|| {
            DispatchError::InvalidState(
                "unimplemented: mailbox export is not yet available in this app".into(),
            )
        })
    }

    /// Fold one accepted chunk's counters into the snapshot.
    fn record_progress(&self, mailbox: &str, exported_in_mailbox: u32, total_exported: u64) {
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.exported_count = u32::try_from(total_exported).unwrap_or(u32::MAX);
        if let Some(row) = snap.mailbox_progress.iter_mut().find(|r| r.name == mailbox) {
            row.exported = exported_in_mailbox;
        }
    }

    /// A finished mailbox's true total under a date range, and the tightened
    /// overall estimate that follows from it.
    fn record_mailbox_total(&self, mailbox: &str, exported_in_mailbox: u32, estimate: u64) {
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.total_count = u32::try_from(estimate).unwrap_or(u32::MAX);
        if let Some(row) = snap.mailbox_progress.iter_mut().find(|r| r.name == mailbox) {
            row.total = exported_in_mailbox;
        }
    }

    /// Mark the session errored on the nest and surface the reason, so a
    /// session-fatal client failure leaves a row that says what happened rather
    /// than one that looks merely paused (§ An unopenable record fails the
    /// session).
    async fn fail_session(
        &self,
        session_id: String,
        generation: u64,
        reason: String,
    ) -> Result<(), DispatchError> {
        let still_current = self
            .session
            .lock()
            .expect("session mutex")
            .as_ref()
            .is_some_and(|s| s.session_id == session_id);
        if !still_current || *self.stop.lock().expect("stop mutex") == StopRequest::Cancel {
            // The user cancelled (or discarded, or started over) while a page
            // was in flight: the nest has already disposed of this session, so
            // the fetch or upload that failed failed *because* of that.
            // Reporting it would paint an "errored" export over the fresh
            // wizard the user is now looking at. The run is the loop's local,
            // so it simply drops; the machine's slot may already hold a NEW
            // run and must not be touched.
            return Ok(());
        }
        // The blob is unopenable without its terminator frame, so the session
        // is disposed of either way; what `fail_export_session` adds over the
        // cancel it replaced is the RECORD. The row reads `errored` carrying
        // this reason, so the user's other devices — and this one after a
        // restart — see an export that FAILED and why, instead of one that
        // merely reads `cancelled` with the reason left on the device that saw
        // it (§ Resume). ⚠ Conditioned on THIS run's generation: a failure here
        // may be this device's alone (a dropped fetch), and if another device
        // has restarted the export since, an unconditional disposal would kill
        // its healthy stream.
        // Any other failure to reach the nest changes nothing the caller can
        // act on — the local error is the report.
        if let Err(ExportSeamError::Superseded) = self
            .nest
            .fail_export_session(session_id.clone(), reason.clone(), Some(generation))
            .await
        {
            return self.superseded(&session_id).await;
        }
        {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            snap.error_log = vec![reason.clone()];
            snap.session_state = Some(ExportSessionState::Errored);
            snap.status = ExportStatus::Idle;
        }
        *self.run.lock().expect("run mutex") = None;
        Err(DispatchError::InvalidState(reason))
    }

    /// Seal one slice of the zstd stream and upload it as the next frame.
    ///
    /// Takes the run's pieces rather than the run: the close path has already
    /// moved the serializer and the stream out by the time it uploads the
    /// stream's tail, and only the sealer is still needed.
    async fn upload_chunk(
        &self,
        session_id: &str,
        generation: u64,
        sealer: &mut fauna_mail::export::ExportBlobSealer,
        chunk: Vec<u8>,
        progress: ExportChunkProgress,
    ) -> Result<(), ExportSeamError> {
        let chunk_idx = sealer.next_chunk_idx();
        let sealed = sealer.seal_chunk(&chunk).map_err(|e| {
            ExportSeamError::Nest(NestError::Rejected(format!("seal export chunk: {e}")))
        })?;
        let frame = with_preamble_on_first(sealer, chunk_idx, sealed);
        self.nest
            .upload_export_chunk(
                session_id.to_string(),
                generation,
                chunk_idx,
                frame,
                progress,
            )
            .await?;
        Ok(())
    }
    /// The chunk-relay drive loop (`mail-export.md` § Export pipeline) — the
    /// export twin of `run_import`. Call once per `Start` / `Resume` dispatch;
    /// the app glue spawns it per platform (native `tokio::spawn`, wasm
    /// `spawn_local`) right after a successful one, exactly as it does for the
    /// import wizard.
    ///
    /// Returns when every selected mailbox is exhausted (session finalized),
    /// when a concurrently-dispatched `Pause`/`Cancel` stopped it at a page
    /// boundary, or on a session-fatal error.
    ///
    /// **Nothing here is skipped.** A record that will not open, a serializer
    /// refusal, a seal failure: each fails the whole session with the reason
    /// (§ An unopenable record fails the session). An archive quietly missing
    /// messages is the one outcome the goal doc puts below failing, because the
    /// user cannot tell — and the cursor is durable, so a failed run is
    /// re-runnable once the cause is fixed.
    async fn run_export_inner(&self) -> Result<(), DispatchError> {
        let Some(mut run) = self.run.lock().expect("run mutex").take() else {
            // No live run: either nothing was started, or this machine found a
            // session another process opened and no Resume has restarted it
            // here yet. The stream cannot be picked up mid-zstd (see
            // `MailExportMachine::run`), so say so rather than producing frames
            // the download would refuse.
            return Err(DispatchError::InvalidState(
                "this export is not running in this app session — press Resume to restart \
                 it here"
                    .into(),
            ));
        };
        let generation = run.generation;
        self.set_status(ExportStatus::Working);
        self.driving
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let _driving = DrivingGuard(&self.driving);

        let mut pending_exported: u64 = 0;

        'mailboxes: while let Some(mailbox) = run.remaining.first().cloned() {
            loop {
                if *self.stop.lock().expect("stop mutex") != StopRequest::None {
                    break 'mailboxes;
                }
                let page = match self
                    .nest
                    .fetch_export_chunk_ciphertext(
                        run.session_id.clone(),
                        mailbox.name.clone(),
                        run.after_uid,
                    )
                    .await
                {
                    Ok(page) => page,
                    Err(e) => {
                        let id = run.session_id.clone();
                        return self
                            .fail_session(
                                id,
                                generation,
                                format!("{}: fetching records: {e}", mailbox.name),
                            )
                            .await;
                    }
                };

                for record in &page.records {
                    // Outside the scope's date range → outside the export.
                    // Decided on the INTERNALDATE the down-leg carries, BEFORE
                    // the record is opened: an out-of-range record this device
                    // cannot open is no reason to fail an export that never
                    // asked for it.
                    if !run.range.admits(record.internal_date) {
                        continue;
                    }
                    let seal_instant = record_seal_instant(record);
                    let body = match run.opener.open(&record.sealed_body, seal_instant) {
                        Ok(body) => body,
                        Err(e) => {
                            let id = run.session_id.clone();
                            return self
                                .fail_session(
                                    id,
                                    generation,
                                    format!(
                                        "{}: uid {} could not be opened on this device ({e}); \
                                         the archive would have been silently short",
                                        record.mailbox, record.uid
                                    ),
                                )
                                .await;
                        }
                    };
                    let message = fauna_mail::export::ExportMessage {
                        mailbox: record.mailbox.clone(),
                        flags: record.flags.clone(),
                        body,
                        internal_date_epoch: record.internal_date,
                        uid: record.uid,
                        uid_validity: mailbox.uid_validity,
                    };
                    let entries = match run.serializer.push(&message) {
                        Ok(entries) => entries,
                        Err(e) => {
                            let id = run.session_id.clone();
                            return self
                                .fail_session(
                                    id,
                                    generation,
                                    format!("{}: uid {}: {e}", record.mailbox, record.uid),
                                )
                                .await;
                        }
                    };
                    for entry in &entries {
                        if let Err(e) = run.stream.push_entry(entry) {
                            let id = run.session_id.clone();
                            return self
                                .fail_session(id, generation, format!("writing the archive: {e}"))
                                .await;
                        }
                    }
                    run.exported += 1;
                    pending_exported += 1;
                    run.exported_here += 1;
                    run.last_processed = format!("{}:{}", record.mailbox, record.uid);
                }

                // Upload whatever full chunks the entries just completed. The
                // stream hands back byte slices of the single zstd stream
                // (§ Container shape), so peak memory stays one message plus
                // one chunk however large the mailbox is.
                for chunk in run.stream.take_full_chunks() {
                    if let Err(e) = self
                        .upload_chunk(
                            &run.session_id,
                            generation,
                            &mut run.sealer,
                            chunk,
                            ExportChunkProgress {
                                exported_delta: pending_exported,
                                last_processed_message_id: run.last_processed.clone(),
                                revised_total_count: run.revised_total.take(),
                            },
                        )
                        .await
                    {
                        let id = run.session_id.clone();
                        if e == ExportSeamError::Superseded {
                            return self.superseded(&id).await;
                        }
                        return self
                            .fail_session(id, generation, format!("uploading a chunk: {e}"))
                            .await;
                    }
                    pending_exported = 0;
                }
                self.record_progress(&mailbox.name, run.exported_here, run.exported);

                run.after_uid = page.next_after_uid;
                if page.mailbox_done {
                    run.remaining.remove(0);
                    if run.range.is_bounded() {
                        // The listing counted the whole mailbox; the range took
                        // some of it. Now that the mailbox is done its true
                        // total is known, so its row stops overstating — and
                        // the overall estimate tightens to what is exported
                        // plus what the unwalked mailboxes could still hold.
                        let estimate = run.exported
                            + run
                                .remaining
                                .iter()
                                .map(|m| u64::from(m.exists))
                                .sum::<u64>();
                        self.record_mailbox_total(&mailbox.name, run.exported_here, estimate);
                        run.revised_total = Some(estimate);
                    }
                    run.after_uid = 0;
                    run.exported_here = 0;
                    continue 'mailboxes;
                }
            }
        }

        let stop = *self.stop.lock().expect("stop mutex");
        if stop == StopRequest::Pause {
            // Every chunk of the page in hand is uploaded, so the session can
            // stop being `running` now. Put the run back first, so a Resume in
            // this same machine picks up the live zstd stream whatever the RPC
            // below answers.
            let session_id = run.session_id.clone();
            *self.run.lock().expect("run mutex") = Some(run);
            match self
                .nest
                .pause_export_session(session_id.clone(), Some(generation))
                .await
            {
                Ok(session) => self.store_session(session),
                Err(ExportSeamError::Superseded) => return self.superseded(&session_id).await,
                Err(e) => return Err(e.into()),
            }
            self.set_status(ExportStatus::Idle);
            return Ok(());
        }
        if stop == StopRequest::Cancel {
            // Cancel already disposed of the session and cleared the machine;
            // putting the run back would resurrect a session whose blob the
            // nest has unlinked.
            self.set_status(ExportStatus::Idle);
            return Ok(());
        }

        // Close the archive: the entries only the end of the run knows (mbox's
        // last mailbox file, Maildir++'s `subscriptions`, EML zip's manifest),
        // then the zip's central directory and the zstd stream's own tail.
        let session_id = run.session_id.clone();
        let tail = match run.serializer.finish() {
            Ok(entries) => entries,
            Err(e) => {
                return self
                    .fail_session(session_id, generation, format!("closing the archive: {e}"))
                    .await;
            }
        };
        for entry in &tail {
            if let Err(e) = run.stream.push_entry(entry) {
                return self
                    .fail_session(session_id, generation, format!("writing the archive: {e}"))
                    .await;
            }
        }
        let final_chunks = match run.stream.finish() {
            Ok(chunks) => chunks,
            Err(e) => {
                return self
                    .fail_session(session_id, generation, format!("closing the archive: {e}"))
                    .await;
            }
        };
        // The exact figure, always: the estimate the nest holds may be the
        // listing's whole-mailbox sum (a ranged export's overstates), and this
        // machine's own snapshot may already have been tightened below it.
        let revised = Some(run.exported);
        for chunk in final_chunks {
            if let Err(e) = self
                .upload_chunk(
                    &session_id,
                    generation,
                    &mut run.sealer,
                    chunk,
                    ExportChunkProgress {
                        exported_delta: pending_exported,
                        last_processed_message_id: run.last_processed.clone(),
                        revised_total_count: revised,
                    },
                )
                .await
            {
                if e == ExportSeamError::Superseded {
                    return self.superseded(&session_id).await;
                }
                return self
                    .fail_session(session_id, generation, format!("uploading a chunk: {e}"))
                    .await;
            }
            pending_exported = 0;
        }

        // The terminator frame: the blob's commitment to its own length, and
        // what an opener demands before it hands back an archive
        // (§ Blob shape on disk). It rides `upload_export_chunk` like any other
        // frame — the nest cannot tell it from a body frame and parses neither.
        let terminator_idx = run.sealer.next_chunk_idx();
        let preamble = run.sealer.preamble();
        let terminator = match run.sealer.finish() {
            Ok(bytes) => bytes,
            Err(e) => {
                return self
                    .fail_session(
                        session_id,
                        generation,
                        format!("sealing the terminator frame: {e}"),
                    )
                    .await;
            }
        };
        // An archive with no body frame at all cannot happen (the zip's central
        // directory alone is a non-empty slice), but the preamble rule is
        // stated for "frame 0", not "the first body frame", so honour it here
        // too rather than lean on that.
        let terminator = if terminator_idx == 0 {
            let mut framed = preamble.to_vec();
            framed.extend_from_slice(&terminator);
            framed
        } else {
            terminator
        };
        if let Err(e) = self
            .nest
            .upload_export_chunk(
                session_id.clone(),
                generation,
                terminator_idx,
                terminator,
                ExportChunkProgress {
                    exported_delta: pending_exported,
                    last_processed_message_id: run.last_processed.clone(),
                    revised_total_count: revised,
                },
            )
            .await
        {
            if e == ExportSeamError::Superseded {
                return self.superseded(&session_id).await;
            }
            return self
                .fail_session(
                    session_id,
                    generation,
                    format!("uploading the terminator frame: {e}"),
                )
                .await;
        }

        let session = match self
            .nest
            .finalize_export_session(session_id.clone(), Some(generation))
            .await
        {
            Ok(session) => session,
            Err(ExportSeamError::Superseded) => return self.superseded(&session_id).await,
            Err(e) => {
                return self
                    .fail_session(
                        session_id,
                        generation,
                        format!("finalizing the session: {e}"),
                    )
                    .await;
            }
        };
        // The run — and with it the session key and the encoder — drops here.
        *self.run.lock().expect("run mutex") = None;
        self.store_session(session);
        self.set_status(ExportStatus::Idle);
        Ok(())
    }

    /// Client-side wizard mutations (no nest round-trip). Returns `true` if the
    /// action was a client-side one (so `dispatch` skips the seam path).
    fn apply_client_action(&self, action: &MailExportAction) -> bool {
        let mut snap = self.inner.lock().expect("snapshot mutex");
        match action {
            MailExportAction::SelectFormat { format } => snap.format = *format,
            MailExportAction::ToggleMailbox { mailbox } => {
                if let Some(m) = snap.mailboxes.iter_mut().find(|m| &m.name == mailbox) {
                    m.selected = !m.selected;
                }
            }
            MailExportAction::SetDateFrom { value } => snap.date_from = value.clone(),
            MailExportAction::SetDateTo { value } => snap.date_to = value.clone(),
            MailExportAction::SetStripHeaders { on } => snap.strip_headers = *on,
            MailExportAction::Next => snap.step = next_step(snap.step),
            MailExportAction::Back => snap.step = prev_step(snap.step),
            _ => return false,
        }
        true
    }
}

/// Clears [`MailExportMachine`]'s `driving` flag on every exit from the loop —
/// return, `?`, or a dropped future — so a later `Pause` never waits on a loop
/// that is no longer there.
struct DrivingGuard<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for DrivingGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Frame 0 carries the 6-byte blob preamble in front of its own bytes
/// (§ Blob shape on disk). The nest appends what it is given and knows nothing
/// about a header, so the preamble has to ride inside an upload, and chunk 0's
/// is the only one where "the start of the file" is where it lands.
fn with_preamble_on_first(
    sealer: &fauna_mail::export::ExportBlobSealer,
    chunk_idx: u64,
    sealed: Vec<u8>,
) -> Vec<u8> {
    if chunk_idx != 0 {
        return sealed;
    }
    let mut framed = sealer.preamble().to_vec();
    framed.extend_from_slice(&sealed);
    framed
}

/// Format → Scope → Confirm (Confirm is the last client-side step; Start drives
/// past it). Progress/Done aren't reachable by Next (they're session-driven).
fn next_step(step: ExportStep) -> ExportStep {
    match step {
        ExportStep::Format => ExportStep::Scope,
        ExportStep::Scope => ExportStep::Confirm,
        other => other,
    }
}

fn prev_step(step: ExportStep) -> ExportStep {
    match step {
        ExportStep::Confirm => ExportStep::Scope,
        ExportStep::Scope => ExportStep::Format,
        other => other,
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl MailExportMachine {
    pub fn snapshot(&self) -> MailExportSnapshot {
        fauna_core::clone_locked(&self.inner, |s| s)
    }

    /// Refresh the user's handle — the name of the archive's root directory
    /// and of the saved file. For an app that learns the handle after building
    /// the machine, or sees it change: the glue calls this before a `Start`,
    /// `Resume` or `Download`, and the next run or download reads it. A run
    /// already under way keeps the handle it started with, so one archive never
    /// names two root directories.
    pub fn set_actor_handle(&self, handle: String) {
        *self.actor_handle.lock().expect("handle mutex") = handle;
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl MailExportMachine {
    /// Initial page load.
    pub async fn hydrate(&self) -> Result<(), DispatchError> {
        self.refresh().await
    }

    /// The chunk-relay drive loop — see [`Self::run_export_inner`] for the
    /// design. Call once per `Start`/`Resume` dispatch (the app glue spawns it
    /// per platform, native `tokio::spawn` / wasm `spawn_local`, right after a
    /// successful one), exactly as the import wizard calls `run_import`.
    pub async fn run_export(&self) -> Result<(), DispatchError> {
        let result = self.run_export_inner().await;
        if let Err(ref e) = result {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            crate::state::set_snapshot_error(&mut snap.error, e.to_string());
            snap.status = ExportStatus::Idle;
        }
        result
    }

    pub async fn dispatch(&self, action: MailExportAction) -> Result<(), DispatchError> {
        // Clear any prior error before the new action runs.
        self.inner.lock().expect("snapshot mutex").error = None;
        // Client-side wizard mutations never touch the seam.
        if self.apply_client_action(&action) {
            return Ok(());
        }
        crate::dispatch_capturing_error!(
            self,
            ExportStatus,
            match action {
                MailExportAction::Refresh => self.refresh().await,
                MailExportAction::Start => self.start().await,
                MailExportAction::Pause => self.pause().await,
                MailExportAction::Resume => self.resume().await,
                MailExportAction::Cancel => self.cancel().await,
                MailExportAction::Download => self.download().await,
                MailExportAction::Discard => self.discard().await,
                // The client-side variants were handled above.
                _ => Ok(()),
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_mail::export::{
        ExportMessage, ExportOptions, build_blob, open_export_blob, serialize_all,
    };
    use fauna_mls::wrapped_blob::{
        ExportSessionKeyBlob, derive_recipient_xwing_keypair, derive_standing_mail_keypairs,
        seal_to_recipient_xwing, unseal_export_session_key,
    };
    use std::collections::BTreeMap;
    use std::sync::{Mutex as StdMutex, OnceLock, Weak};

    const SESSION_ID: &str = "5a0c3c1e-8f4b-4c2e-9d7a-3b6f1e2d4c5a";
    const MSEK: [u8; 32] = [0x5e; 32];
    const ACTOR: [u8; 32] = [0xa1; 32];
    const HANDLE: &str = "alice";

    /// One message as it rests on the nest: a **bare inner** sealed envelope,
    /// exactly the shape `fetch_export_chunk_ciphertext` hands back.
    #[derive(Clone)]
    struct Stored {
        uid: u32,
        internal_date: i64,
        /// The nest's seal instant (`0` = unknown, a failed clock read).
        stored_at: i64,
        flags: Vec<String>,
        sealed: Vec<u8>,
        /// The plaintext this record opens to — what the archive must contain.
        plain: Vec<u8>,
    }

    fn body(subject: &str) -> Vec<u8> {
        format!(
            "From: sender@example.com\r\nMessage-ID: <{subject}@example.com>\r\n\
             Subject: {subject}\r\n\r\nBody of {subject}.\r\n"
        )
        .into_bytes()
    }

    fn sealed_to(msek: &[u8; 32], plain: &[u8]) -> Vec<u8> {
        seal_to_recipient_xwing(plain, &derive_recipient_xwing_keypair(msek).public)
            .expect("seal")
            .to_canonical_bytes()
            .expect("encode inner envelope")
    }

    fn stored(uid: u32, internal_date: i64, subject: &str) -> Stored {
        let plain = body(subject);
        Stored {
            uid,
            internal_date,
            stored_at: 0,
            flags: vec!["\\Seen".into()],
            sealed: sealed_to(&MSEK, &plain),
            plain,
        }
    }

    /// A record sealed the way the nest's ingest seals today: to the **mail
    /// epoch** of its seal instant `stored_at`, not to the standing key —
    /// whatever its own `internal_date` says.
    fn stored_in_epoch(uid: u32, internal_date: i64, stored_at: i64, subject: &str) -> Stored {
        let plain = body(subject);
        let root = fauna_mls::wrapped_blob::derive_mail_epoch_root(&MSEK);
        let epoch = fauna_mls::wrapped_blob::mail_sealing_epoch_of(stored_at as u64);
        let kp =
            fauna_mls::wrapped_blob::derive_recipient_epoch_xwing_keypair_from_root(&root, epoch);
        Stored {
            uid,
            internal_date,
            stored_at,
            flags: vec!["\\Seen".into()],
            sealed: seal_to_recipient_xwing(&plain, &kp.public)
                .expect("seal under the epoch key")
                .to_canonical_bytes()
                .expect("encode inner envelope"),
            plain,
        }
    }

    #[derive(Default)]
    struct FakeState {
        session: Option<ExportSessionView>,
        wrapped_key: Option<Vec<u8>>,
        started_total: Option<u64>,
        blob: Vec<u8>,
        next_chunk_idx: u64,
        fetches: usize,
        cancelled: bool,
        /// The reason `fail_export_session` recorded, if it was called — the
        /// row's `error_reason`, which a cancel has no place to put.
        failed: Option<String>,
        finalized: bool,
        /// The session's current stream generation (§ Resume).
        generation: u64,
    }

    /// In-memory nest modelling the **built** backend: the caller-scoped
    /// listing, the UID-paged down-leg with `mailbox_done` as the only end
    /// signal, an append-only blob that refuses any `chunk_idx` but the next
    /// one, and the session transitions. It parses no frame and holds no key —
    /// the same blindness the real nest has.
    struct FakeNest {
        listing: Vec<ExportMailboxCount>,
        records: BTreeMap<String, Vec<Stored>>,
        page_size: usize,
        state: StdMutex<FakeState>,
        /// Simulate the user pressing Pause while the loop is mid-run: after
        /// this many fetches the fake requests a pause on the machine, as a
        /// concurrent `dispatch(Pause)` would.
        pause_after_fetches: Option<usize>,
        /// The Cancel twin of `pause_after_fetches`.
        cancel_after_fetches: Option<usize>,
        /// Simulate the user's OTHER device restarting the export while this
        /// machine's loop is mid-run: during this fetch the fake opens a new
        /// stream generation, exactly as `restart_export_session` would.
        restart_after_fetches: Option<usize>,
        /// Fail this fetch (after any restart hook ran) — a device-local fault.
        fail_fetch: Option<usize>,
        machine: OnceLock<Weak<MailExportMachine>>,
    }

    impl FakeNest {
        fn new(mailboxes: &[(&str, Vec<Stored>)]) -> Self {
            let mut listing: Vec<ExportMailboxCount> = mailboxes
                .iter()
                .map(|(name, recs)| ExportMailboxCount {
                    name: (*name).to_string(),
                    exists: recs.len() as u32,
                    uid_validity: 7,
                })
                .collect();
            // The real handler's order: ascending by the name's raw bytes.
            listing.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
            Self {
                listing,
                records: mailboxes
                    .iter()
                    .map(|(n, r)| ((*n).to_string(), r.clone()))
                    .collect(),
                page_size: 2,
                state: StdMutex::new(FakeState::default()),
                pause_after_fetches: None,
                cancel_after_fetches: None,
                restart_after_fetches: None,
                fail_fetch: None,
                machine: OnceLock::new(),
            }
        }

        fn names(names: &[&str]) -> Self {
            Self::new(&names.iter().map(|n| (*n, Vec::new())).collect::<Vec<_>>())
        }

        fn transition(
            &self,
            to: ExportSessionState,
            as_driver_of: Option<u64>,
        ) -> Result<ExportSessionView, ExportSeamError> {
            let mut st = self.state.lock().unwrap();
            // The real handler's order: a stale generation is decided before
            // anything else, the already-there convergence arm included.
            if as_driver_of.is_some_and(|g| g != st.generation) {
                return Err(ExportSeamError::Superseded);
            }
            let s = st
                .session
                .as_mut()
                .ok_or(NestError::Rejected("no session".into()))?;
            s.state = to;
            Ok(s.clone())
        }

        /// What `restart_export_session` does to the row and the blob: a new
        /// generation, an EMPTY blob, a zeroed cursor and counters, the fresh
        /// wrapped key, `running`.
        fn restart(
            &self,
            wrapped_session_key: Vec<u8>,
            total_count: u64,
        ) -> Result<ExportSessionView, ExportSeamError> {
            let mut st = self.state.lock().unwrap();
            if !matches!(
                st.session.as_ref().map(|s| s.state),
                Some(ExportSessionState::Running | ExportSessionState::Paused)
            ) {
                return Err(NestError::Rejected("not in flight".into()).into());
            }
            st.generation += 1;
            st.blob.clear();
            st.next_chunk_idx = 0;
            st.wrapped_key = Some(wrapped_session_key);
            let generation = st.generation;
            let s = st.session.as_mut().expect("session");
            s.state = ExportSessionState::Running;
            s.stream_generation = generation;
            s.exported_count = 0;
            s.total_count = total_count as u32;
            Ok(s.clone())
        }
    }

    fn view(state: ExportSessionState, total: u32) -> ExportSessionView {
        ExportSessionView {
            session_id: SESSION_ID.into(),
            state,
            format: ExportFormat::Mbox,
            exported_count: 0,
            skipped_count: 0,
            errored_count: 0,
            total_count: total,
            error_reason: String::new(),
            blob_bytes: None,
            download_url: String::new(),
            wrapped_session_key: Vec::new(),
            stream_generation: 0,
            scope: None,
        }
    }

    #[async_trait]
    impl MailExportNest for FakeNest {
        async fn list_own_mailboxes(&self) -> Result<Vec<ExportMailboxCount>, NestError> {
            Ok(self.listing.clone())
        }
        async fn list_export_sessions(&self) -> Result<Vec<ExportSessionView>, NestError> {
            Ok(self
                .state
                .lock()
                .unwrap()
                .session
                .clone()
                .into_iter()
                .collect())
        }
        async fn start_export_session(
            &self,
            format: ExportFormat,
            scope: ExportScope,
            wrapped_session_key: Vec<u8>,
            total_count: u64,
        ) -> Result<ExportSessionView, NestError> {
            if wrapped_session_key.is_empty() {
                return Err(NestError::Rejected("wrapped_session_key required".into()));
            }
            let mut st = self.state.lock().unwrap();
            let mut s = view(ExportSessionState::Running, total_count as u32);
            s.format = format;
            // The row hands the scope back verbatim — what a cold resume reads.
            s.scope = Some(scope);
            st.session = Some(s.clone());
            st.wrapped_key = Some(wrapped_session_key);
            st.started_total = Some(total_count);
            Ok(s)
        }
        async fn fetch_export_chunk_ciphertext(
            &self,
            _session_id: String,
            mailbox: String,
            after_uid: u32,
        ) -> Result<ExportFetchPage, NestError> {
            let fetches = {
                let mut st = self.state.lock().unwrap();
                if st.session.as_ref().map(|s| s.state) != Some(ExportSessionState::Running) {
                    return Err(NestError::Rejected("session is not running".into()));
                }
                st.fetches += 1;
                st.fetches
            };
            // A real concurrent `dispatch(Pause)`, not a hand-set flag: it goes
            // through the machine's own pause path (stop request, the nest
            // transition, the snapshot), exactly as a user's click would while
            // this fetch is in flight.
            if self.pause_after_fetches == Some(fetches)
                && let Some(m) = self.machine.get().and_then(Weak::upgrade)
            {
                m.dispatch(MailExportAction::Pause)
                    .await
                    .expect("a concurrent pause lands");
            }
            if self.cancel_after_fetches == Some(fetches)
                && let Some(m) = self.machine.get().and_then(Weak::upgrade)
            {
                m.dispatch(MailExportAction::Cancel)
                    .await
                    .expect("a concurrent cancel lands");
            }
            if self.restart_after_fetches == Some(fetches) {
                self.restart(b"the other device's key".to_vec(), 0)
                    .expect("the other device's restart lands");
            }
            if self.fail_fetch == Some(fetches) {
                return Err(NestError::Transient("connection dropped".into()));
            }
            let all = self.records.get(&mailbox).cloned().unwrap_or_default();
            let rest: Vec<Stored> = all.into_iter().filter(|r| r.uid > after_uid).collect();
            let page: Vec<Stored> = rest.iter().take(self.page_size).cloned().collect();
            Ok(ExportFetchPage {
                next_after_uid: page.last().map_or(after_uid, |r| r.uid),
                mailbox_done: rest.len() <= self.page_size,
                records: page
                    .into_iter()
                    .map(|r| ExportRecord {
                        mailbox: mailbox.clone(),
                        uid: r.uid,
                        flags: r.flags,
                        internal_date: r.internal_date,
                        stored_at: r.stored_at,
                        sealed_body: r.sealed,
                    })
                    .collect(),
            })
        }
        async fn upload_export_chunk(
            &self,
            _session_id: String,
            stream_generation: u64,
            chunk_idx: u64,
            sealed_chunk: Vec<u8>,
            progress: ExportChunkProgress,
        ) -> Result<ExportUploadAck, ExportSeamError> {
            let mut st = self.state.lock().unwrap();
            // Decided before the state and the index, as the real reservation
            // does: a superseded driver hears that and nothing else.
            if stream_generation != st.generation {
                return Err(ExportSeamError::Superseded);
            }
            // The real handler's guard: a frame lands only on a `running`
            // session. Without it the fake would hide the race the loop's
            // pause handoff exists for.
            if st.session.as_ref().map(|s| s.state) != Some(ExportSessionState::Running) {
                return Err(NestError::Rejected("session is not running".into()).into());
            }
            if chunk_idx != st.next_chunk_idx {
                return Err(NestError::Rejected(format!(
                    "chunk_idx {chunk_idx}, expected {}",
                    st.next_chunk_idx
                ))
                .into());
            }
            st.blob.extend_from_slice(&sealed_chunk);
            st.next_chunk_idx += 1;
            let blob_bytes = st.blob.len() as u64;
            let next_chunk_idx = st.next_chunk_idx;
            let s = st.session.as_mut().expect("session");
            s.exported_count += progress.exported_delta as u32;
            // The real fold's `COALESCE(revised, total_count)`.
            if let Some(revised) = progress.revised_total_count {
                s.total_count = revised as u32;
            }
            Ok(ExportUploadAck {
                blob_bytes,
                next_chunk_idx,
                exported_count: u64::from(s.exported_count),
            })
        }
        async fn pause_export_session(
            &self,
            _session_id: String,
            as_driver_of: Option<u64>,
        ) -> Result<ExportSessionView, ExportSeamError> {
            self.transition(ExportSessionState::Paused, as_driver_of)
        }
        async fn resume_export_session(
            &self,
            _session_id: String,
            as_driver_of: Option<u64>,
        ) -> Result<ExportSessionView, ExportSeamError> {
            self.transition(ExportSessionState::Running, as_driver_of)
        }
        async fn restart_export_session(
            &self,
            _session_id: String,
            wrapped_session_key: Vec<u8>,
            total_count: u64,
        ) -> Result<ExportSessionView, ExportSeamError> {
            self.restart(wrapped_session_key, total_count)
        }
        async fn cancel_export_session(
            &self,
            _session_id: String,
            as_driver_of: Option<u64>,
        ) -> Result<(), ExportSeamError> {
            let mut st = self.state.lock().unwrap();
            if as_driver_of.is_some_and(|g| g != st.generation) {
                return Err(ExportSeamError::Superseded);
            }
            st.session = None;
            st.blob.clear();
            st.cancelled = true;
            Ok(())
        }
        async fn finalize_export_session(
            &self,
            _session_id: String,
            as_driver_of: Option<u64>,
        ) -> Result<ExportSessionView, ExportSeamError> {
            let mut st = self.state.lock().unwrap();
            if as_driver_of.is_some_and(|g| g != st.generation) {
                return Err(ExportSeamError::Superseded);
            }
            st.finalized = true;
            let blob_bytes = st.blob.len() as u64;
            // The real projection carries the row's stored wrapped key on every
            // session view (`rpc_glue::project_export_session`), because the
            // client that downloads need not be the one that exported. Without
            // it here the download leg would be untestable against this fake
            // for a reason the production path does not have.
            let wrapped = st.wrapped_key.clone().unwrap_or_default();
            let s = st.session.as_mut().expect("session");
            s.state = ExportSessionState::Completed;
            s.blob_bytes = Some(blob_bytes);
            s.download_url = format!("/api/v1/export/{SESSION_ID}");
            s.wrapped_session_key = wrapped;
            Ok(s.clone())
        }
        async fn fail_export_session(
            &self,
            _session_id: String,
            reason: String,
            as_driver_of: Option<u64>,
        ) -> Result<(), ExportSeamError> {
            let mut st = self.state.lock().unwrap();
            if as_driver_of.is_some_and(|g| g != st.generation) {
                return Err(ExportSeamError::Superseded);
            }
            // The real handler: the transition records the reason and the
            // partial blob goes the way a cancel's does — but the ROW survives,
            // which is the whole point of the kind.
            st.blob.clear();
            st.failed = Some(reason);
            if let Some(s) = st.session.as_mut() {
                s.state = ExportSessionState::Errored;
            }
            Ok(())
        }
        async fn discard_export_blob(&self, _session_id: String) -> Result<(), NestError> {
            let mut st = self.state.lock().unwrap();
            st.session = None;
            st.blob.clear();
            Ok(())
        }
    }

    /// Key custody over a fixed test MSEK, through the SAME production mint and
    /// opener `MailSettingsMachine` uses — so a round-trip here exercises the
    /// real wrap, the real standing-set trial and the real frame seal.
    struct FakeCustody {
        msek: [u8; 32],
    }

    #[async_trait]
    impl MailExportKeyCustody for FakeCustody {
        async fn mint_export_session_key(&self) -> Result<MintedExportSessionKey, DispatchError> {
            mint_export_session_key_for(&self.msek, &ACTOR)
        }
        async fn export_record_opener(&self) -> Result<Arc<dyn MailRecordOpening>, DispatchError> {
            Ok(Arc::new(StandingKeyRecordOpener::from_msek_history(
                &[self.msek],
                &[],
            )))
        }
        async fn unwrap_export_session_key(
            &self,
            wrapped: &[u8],
        ) -> Result<zeroize::Zeroizing<[u8; 32]>, DispatchError> {
            let blob = ExportSessionKeyBlob::from_canonical_bytes(wrapped)
                .map_err(|e| DispatchError::InvalidState(format!("decode wrapped key: {e}")))?;
            let key =
                unseal_export_session_key(&blob, &derive_standing_mail_keypairs(&[self.msek]))
                    .map_err(|e| DispatchError::InvalidState(format!("unwrap: {e}")))?;
            Ok(zeroize::Zeroizing::new(key))
        }
    }

    /// The platform delivery half, in memory: `open_download` replays whatever
    /// the fake nest holds for the session's blob, and `create_archive` collects
    /// what the opener writes. Nothing here parses a frame — the point is that
    /// everything between the two is shared Rust.
    #[derive(Default)]
    struct FakeDeliveryState {
        /// What the "download" yields. `None` ⇒ take the nest's current blob.
        override_body: Option<Vec<u8>>,
        /// The bytes `finish()` accepted, and the name they were saved under.
        saved: Option<(String, Vec<u8>)>,
        /// Sinks opened but never finished — an archive the opener refused.
        abandoned: usize,
        requested_url: Option<String>,
    }

    struct FakeDelivery {
        nest: Arc<FakeNest>,
        state: Arc<Mutex<FakeDeliveryState>>,
        /// Slice size the body is handed back in — small on purpose, so the
        /// opener has to reassemble frames across arrivals.
        slice: usize,
    }

    impl FakeDelivery {
        fn new(nest: Arc<FakeNest>) -> (Arc<Self>, Arc<Mutex<FakeDeliveryState>>) {
            let state = Arc::new(Mutex::new(FakeDeliveryState::default()));
            (
                Arc::new(Self {
                    nest,
                    state: Arc::clone(&state),
                    slice: 7,
                }),
                state,
            )
        }
    }

    #[async_trait]
    impl ExportArchiveDelivery for FakeDelivery {
        async fn open_download(
            &self,
            download_url: String,
        ) -> Result<Box<dyn SealedBlobStream>, DispatchError> {
            let mut st = self.state.lock().unwrap();
            st.requested_url = Some(download_url);
            let body = st
                .override_body
                .clone()
                .unwrap_or_else(|| self.nest.state.lock().unwrap().blob.clone());
            Ok(Box::new(FakeBlobStream {
                body,
                at: 0,
                slice: self.slice,
            }))
        }
        async fn create_archive(
            &self,
            file_name: String,
        ) -> Result<Box<dyn ArchiveFileSink>, DispatchError> {
            self.state.lock().unwrap().abandoned += 1;
            Ok(Box::new(FakeArchiveFile {
                name: file_name,
                bytes: Vec::new(),
                state: Arc::clone(&self.state),
            }))
        }
    }

    struct FakeBlobStream {
        body: Vec<u8>,
        at: usize,
        slice: usize,
    }

    #[async_trait]
    impl SealedBlobStream for FakeBlobStream {
        async fn next_slice(&mut self) -> Result<Option<Vec<u8>>, DispatchError> {
            if self.at >= self.body.len() {
                return Ok(None);
            }
            let end = (self.at + self.slice).min(self.body.len());
            let out = self.body[self.at..end].to_vec();
            self.at = end;
            Ok(Some(out))
        }
    }

    struct FakeArchiveFile {
        name: String,
        bytes: Vec<u8>,
        state: Arc<Mutex<FakeDeliveryState>>,
    }

    #[async_trait]
    impl ArchiveFileSink for FakeArchiveFile {
        async fn write(&mut self, bytes: &[u8]) -> Result<(), DispatchError> {
            self.bytes.extend_from_slice(bytes);
            Ok(())
        }
        async fn finish(self: Box<Self>) -> Result<String, DispatchError> {
            let mut st = self.state.lock().unwrap();
            st.abandoned -= 1;
            st.saved = Some((self.name.clone(), self.bytes));
            Ok(format!("/downloads/{}", self.name))
        }
    }

    fn machine_over(nest: Arc<FakeNest>) -> Arc<MailExportMachine> {
        machine_and_delivery(nest).0
    }

    /// The machine plus a handle on what its delivery half saw — one builder,
    /// so every existing test keeps the download leg wired rather than growing
    /// a second wiring that could drift from it.
    fn machine_and_delivery(
        nest: Arc<FakeNest>,
    ) -> (Arc<MailExportMachine>, Arc<Mutex<FakeDeliveryState>>) {
        let (delivery, seen) = FakeDelivery::new(Arc::clone(&nest));
        let m = Arc::new(
            MailExportMachine::new(
                nest.clone(),
                Arc::new(FakeCustody { msek: MSEK }),
                delivery,
                HANDLE,
            )
            // Small enough that every fixture run spans many frames, which
            // is what exercises the index sequencing and the preamble rule.
            .with_chunk_bytes(64),
        );
        let _ = nest.machine.set(Arc::downgrade(&m));
        (m, seen)
    }

    fn machine(mailboxes: &[&str]) -> Arc<MailExportMachine> {
        machine_over(Arc::new(FakeNest::names(mailboxes)))
    }

    async fn walk_to_confirm(m: &MailExportMachine) {
        m.hydrate().await.unwrap();
        m.dispatch(MailExportAction::Next).await.unwrap();
        m.dispatch(MailExportAction::Next).await.unwrap();
    }

    /// Unwrap the session key the way a second client of the same user would —
    /// from the wrapped bytes the nest stored, under the standing set — then
    /// open the blob and return the plaintext `.zip.zst`.
    fn open_downloaded(nest: &FakeNest, format: ExportFormat) -> Vec<u8> {
        let st = nest.state.lock().unwrap();
        let wrapped =
            ExportSessionKeyBlob::from_canonical_bytes(st.wrapped_key.as_ref().expect("wrapped"))
                .expect("decode wrapped key");
        let key = unseal_export_session_key(&wrapped, &derive_standing_mail_keypairs(&[MSEK]))
            .expect("the user's own standing set opens the session key");
        open_export_blob(&key, SESSION_ID, format.wire_name(), &st.blob)
            .expect("the uploaded frames open, terminator included")
    }

    /// What the archive must be: the same plaintext, serialized in one pass.
    fn expected_blob(format: ExportFormat, mailboxes: &[(&str, Vec<Stored>)]) -> Vec<u8> {
        let mut names: Vec<&(&str, Vec<Stored>)> = mailboxes.iter().collect();
        names.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let messages: Vec<ExportMessage> = names
            .into_iter()
            .flat_map(|(name, recs)| {
                recs.iter().map(move |r| ExportMessage {
                    mailbox: (*name).to_string(),
                    flags: r.flags.clone(),
                    body: r.plain.clone(),
                    internal_date_epoch: r.internal_date,
                    uid: r.uid,
                    uid_validity: 7,
                })
            })
            .collect();
        build_blob(
            &serialize_all(format, ExportOptions::new(HANDLE), &messages).expect("serialize"),
        )
        .expect("build blob")
    }

    fn corpus() -> Vec<(&'static str, Vec<Stored>)> {
        vec![
            // Deliberately NOT in raw-byte order here: the listing sorts, and
            // the run must follow the listing.
            (
                "INBOX",
                vec![
                    stored(1, 1_700_000_000, "hello"),
                    // A UID gap, so a short page mid-mailbox cannot be read as
                    // its end.
                    stored(5, 1_700_000_100, "gap"),
                    // An IMPORTED message: its source's old date on a later UID.
                    stored(9, 900_000_000, "imported"),
                ],
            ),
            ("Archive", vec![stored(3, 1_600_000_000, "archived")]),
            ("Sent", vec![stored(2, 1_700_000_050, "sent")]),
        ]
    }

    // ── the download-and-open leg (§ Download flow) ────────────────────

    /// The other end of the pipeline: after a finished run, `Download` GETs the
    /// blob, unwraps the row's key under the account's standing set, opens the
    /// frames in order and writes out the recovered `.zip.zst` — byte-for-byte
    /// the archive the run built.
    ///
    /// The key deliberately comes from the **row**, not from the run's memory:
    /// § Download flow's client need not be the one that exported, and the fake
    /// custody unwraps the stored bytes exactly as a second device would.
    #[tokio::test]
    async fn download_saves_the_archive_the_run_built() {
        let corpus = corpus();
        let nest = Arc::new(FakeNest::new(&corpus));
        let (m, seen) = machine_and_delivery(nest.clone());
        walk_to_confirm(&m).await;
        m.dispatch(MailExportAction::Start).await.unwrap();
        m.run_export().await.expect("the run completes");

        m.dispatch(MailExportAction::Download).await.unwrap();
        let snap = m.snapshot();
        assert!(snap.error.is_none(), "{:?}", snap.error);

        let st = seen.lock().unwrap();
        let (name, bytes) = st.saved.clone().expect("the archive was saved");
        assert_eq!(
            bytes,
            open_downloaded(&nest, ExportFormat::Mbox),
            "the saved bytes are the opened blob"
        );
        assert_eq!(
            bytes,
            expected_blob(ExportFormat::Mbox, &corpus),
            "and that is the archive a one-shot serialization produces"
        );
        // § Compression wrapper's name, and NOT the session id, which
        // § Architectural rules calls sensitive.
        assert!(name.starts_with("fauna-export-"), "{name}");
        assert!(
            name.ends_with("-mbox-") || name.contains("-mbox-"),
            "{name}"
        );
        assert!(name.ends_with(".zip.zst"), "{name}");
        assert!(
            !name.contains(SESSION_ID),
            "the file name must not leak the session id: {name}"
        );
        // The URL came from the row, not from anything the client composed.
        assert_eq!(
            st.requested_url.as_deref(),
            Some(format!("/api/v1/export/{SESSION_ID}").as_str())
        );
        assert_eq!(snap.saved_archive_path, format!("/downloads/{name}"));
    }

    /// § Architectural rules' no-partial-blob-download rule, at its one call
    /// site: a body that stops before the terminator is refused, and — because
    /// the terminator check runs *before* the sink is closed — nothing is left
    /// behind that reads as the user's whole mailbox.
    #[tokio::test]
    async fn a_download_without_its_terminator_saves_nothing() {
        let corpus = corpus();
        let nest = Arc::new(FakeNest::new(&corpus));
        let (m, seen) = machine_and_delivery(nest.clone());
        walk_to_confirm(&m).await;
        m.dispatch(MailExportAction::Start).await.unwrap();
        m.run_export().await.expect("the run completes");

        // Chop the terminator frame (and then some) off what the "download"
        // yields, leaving body frames that all open perfectly well.
        {
            let full = nest.state.lock().unwrap().blob.clone();
            let cut = full.len() - 60;
            seen.lock().unwrap().override_body = Some(full[..cut].to_vec());
        }

        let err = m
            .dispatch(MailExportAction::Download)
            .await
            .expect_err("a blob without its terminator must be refused");
        assert!(
            format!("{err}").contains("terminator"),
            "the refusal must name what is missing: {err}"
        );
        let st = seen.lock().unwrap();
        assert!(
            st.saved.is_none(),
            "no archive may be saved from a truncated blob"
        );
        assert_eq!(st.abandoned, 1, "the sink was opened and never finished");
        assert_eq!(m.snapshot().saved_archive_path, "");
    }

    /// A session still running has no download link, and asking for one says so
    /// rather than reaching for a blob the nest would 404 anyway.
    #[tokio::test]
    async fn download_before_the_export_finishes_is_refused() {
        let nest = Arc::new(FakeNest::new(&corpus()));
        let (m, seen) = machine_and_delivery(nest.clone());
        walk_to_confirm(&m).await;
        m.dispatch(MailExportAction::Start).await.unwrap();

        let err = m
            .dispatch(MailExportAction::Download)
            .await
            .expect_err("a running export has nothing to download");
        assert!(format!("{err}").contains("not finished"), "{err}");
        assert!(
            seen.lock().unwrap().requested_url.is_none(),
            "nothing was fetched"
        );
    }

    /// An app whose glue has no download half says so on `error-message`
    /// instead of pretending to have saved something — the `key_custody` shape.
    #[tokio::test]
    async fn an_app_without_the_delivery_half_refuses_honestly() {
        let nest = Arc::new(FakeNest::new(&corpus()));
        let m = Arc::new(
            MailExportMachine::without_archive_delivery(
                nest.clone(),
                Arc::new(FakeCustody { msek: MSEK }),
                HANDLE,
            )
            .with_chunk_bytes(64),
        );
        let _ = nest.machine.set(Arc::downgrade(&m));
        walk_to_confirm(&m).await;
        m.dispatch(MailExportAction::Start).await.unwrap();
        m.run_export().await.expect("the run completes");

        let err = m
            .dispatch(MailExportAction::Download)
            .await
            .expect_err("no delivery half ⇒ no silent no-op");
        assert!(format!("{err}").contains("not yet available"), "{err}");
        assert_eq!(m.snapshot().saved_archive_path, "");
    }

    /// The handle is live, not baked at construction: an app that learns it
    /// after building the machine (a fresh sign-in fetches it asynchronously),
    /// or sees the user change it, refreshes it, and the next download is named
    /// for the handle the user has now.
    #[tokio::test]
    async fn a_refreshed_handle_names_the_next_download() {
        let corpus = corpus();
        let nest = Arc::new(FakeNest::new(&corpus));
        let (m, seen) = machine_and_delivery(nest.clone());
        walk_to_confirm(&m).await;
        m.dispatch(MailExportAction::Start).await.unwrap();
        m.run_export().await.expect("the run completes");

        m.set_actor_handle("renamed".into());
        m.dispatch(MailExportAction::Download).await.unwrap();
        let (name, _bytes) = seen.lock().unwrap().saved.clone().expect("saved");
        assert!(name.starts_with("fauna-export-renamed-mbox-"), "{name}");
    }

    /// § Compression wrapper's name is derived, not fixed: the format token and
    /// the download date both ride in it, and a handle that is not a safe path
    /// component is encoded exactly as the archive's root directory encodes it.
    #[test]
    fn the_download_file_name_carries_the_format_and_the_day() {
        // 2026-09-22T00:00:00Z plus a few hours — the day must not slide.
        let secs = 1_790_000_000;
        let (y, mo, d) = fauna_core::caltime::civil_from_days(secs / 86_400);
        assert_eq!(
            archive_file_name("alice", ExportFormat::Mbox, secs),
            format!("fauna-export-alice-mbox-{y:04}-{mo:02}-{d:02}.zip.zst")
        );
        assert_eq!(
            archive_file_name("alice", ExportFormat::MaildirPlus, secs),
            format!("fauna-export-alice-maildir-{y:04}-{mo:02}-{d:02}.zip.zst")
        );
        assert_eq!(
            archive_file_name("alice", ExportFormat::EmlZip, secs),
            format!("fauna-export-alice-eml-zip-{y:04}-{mo:02}-{d:02}.zip.zst")
        );
        // A handle with a separator in it cannot produce a name with one.
        let hostile = archive_file_name("a/b", ExportFormat::Mbox, secs);
        assert!(!hostile.contains('/'), "{hostile}");
        assert!(!hostile.contains('\\'), "{hostile}");
        // And an empty handle still yields a saveable name.
        assert!(
            archive_file_name("", ExportFormat::Mbox, secs).starts_with("fauna-export-export-")
        );
    }

    // ── the pipeline ───────────────────────────────────────────────────

    /// The flow this row makes true, minus the transport: Start mints and
    /// wraps the key, the loop opens every record, serializes it into the one
    /// stream, seals the slices and uploads them, then terminates and
    /// finalizes — and the blob, opened the way any of the user's clients
    /// would open it, is byte-for-byte the archive a one-shot serialization of
    /// the same mail produces (§ Container shape's determinism contract).
    #[tokio::test]
    async fn run_export_produces_a_blob_that_opens_to_the_exact_plaintext_archive() {
        let corpus = corpus();
        let nest = Arc::new(FakeNest::new(&corpus));
        let m = machine_over(nest.clone());
        walk_to_confirm(&m).await;
        m.dispatch(MailExportAction::Start).await.unwrap();
        m.run_export().await.expect("the run completes");

        let snap = m.snapshot();
        assert_eq!(snap.step, ExportStep::Done);
        assert_eq!(snap.session_state, Some(ExportSessionState::Completed));
        assert_eq!(snap.exported_count, 5);
        assert!(snap.error.is_none(), "{:?}", snap.error);
        {
            let st = nest.state.lock().unwrap();
            assert!(st.finalized);
            assert!(
                st.next_chunk_idx > 2,
                "a 64-byte chunk size spans many frames"
            );
            assert_eq!(
                st.started_total,
                Some(5),
                "the listing's counts are the estimate"
            );
        }
        assert_eq!(
            open_downloaded(&nest, ExportFormat::Mbox),
            expected_blob(ExportFormat::Mbox, &corpus),
        );
    }

    /// A session the user disposed of on another device does not linger here:
    /// the next refresh drops the held session and the wizard is back on its
    /// Format step, with no download pointing at a blob that is gone.
    #[tokio::test]
    async fn a_session_discarded_elsewhere_is_dropped_on_the_next_refresh() {
        let corpus = corpus();
        let nest = Arc::new(FakeNest::new(&corpus));
        let m = machine_over(nest.clone());
        walk_to_confirm(&m).await;
        m.dispatch(MailExportAction::Start).await.unwrap();
        m.run_export().await.expect("the run completes");
        assert_eq!(m.snapshot().step, ExportStep::Done);

        // Another device presses Discard: the nest no longer lists it.
        nest.state.lock().unwrap().session = None;
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.step, ExportStep::Format, "{snap:?}");
        assert!(snap.download_url.is_empty(), "{snap:?}");
    }

    /// A refresh with no session anywhere leaves a wizard in progress alone:
    /// the choices a user has made on the Format/Scope steps survive it.
    #[tokio::test]
    async fn a_refresh_without_any_session_keeps_the_wizard_where_it_is() {
        let m = machine(&["INBOX", "Archive"]);
        m.hydrate().await.unwrap();
        m.dispatch(MailExportAction::SelectFormat {
            format: ExportFormat::EmlZip,
        })
        .await
        .unwrap();
        m.dispatch(MailExportAction::Next).await.unwrap();
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.step, ExportStep::Scope);
        assert_eq!(snap.format, ExportFormat::EmlZip);
    }

    /// A record is opened by its **seal instant**, not its date. Imported mail
    /// (dated years back) and mail delivered in a later weekly epoch than its
    /// `Date:` both rest sealed to the epoch they were STORED in; the epoch
    /// chain scans only backward from its target, so a target taken from
    /// INTERNALDATE misses them and the export fails. Pinned with a message
    /// dated three epochs before it was stored, and one dated the day before
    /// an epoch boundary it was delivered just after — the shape that took the
    /// e2e journey red the week its fixture's date aged past a boundary.
    #[tokio::test]
    async fn a_record_sealed_after_the_epoch_of_its_date_still_opens() {
        let week = fauna_mls::wrapped_blob::MAIL_SEALING_EPOCH_SECS as i64;
        let boundary = 2_926 * week; // an epoch start, 2026-02
        let corpus = vec![(
            "INBOX",
            vec![
                // Imported: dated three epochs back, stored now.
                stored_in_epoch(1, boundary - 3 * week + 60, boundary + 60, "imported-old"),
                // Dated the day before the boundary, delivered just after it.
                stored_in_epoch(2, boundary - 86_400, boundary + 3_600, "late-delivery"),
            ],
        )];
        let nest = Arc::new(FakeNest::new(&corpus));
        let m = machine_over(nest.clone());
        walk_to_confirm(&m).await;
        m.dispatch(MailExportAction::Start).await.unwrap();
        m.run_export()
            .await
            .expect("every record opens by its seal instant");
        assert!(m.snapshot().error.is_none(), "{:?}", m.snapshot().error);
        assert_eq!(
            open_downloaded(&nest, ExportFormat::Mbox),
            expected_blob(ExportFormat::Mbox, &corpus)
        );
    }

    /// A failed clock read reports no seal instant (`stored_at` 0): the opener falls
    /// back to INTERNALDATE, which is still right for a record sealed in the
    /// epoch its date names.
    #[test]
    fn the_seal_instant_falls_back_to_the_date_when_the_nest_reports_none() {
        let record = |stored_at| ExportRecord {
            mailbox: "INBOX".into(),
            uid: 1,
            flags: Vec::new(),
            internal_date: 1_700_000_000,
            stored_at,
            sealed_body: Vec::new(),
        };
        assert_eq!(record_seal_instant(&record(1_790_000_000)), 1_790_000_000);
        assert_eq!(record_seal_instant(&record(0)), 1_700_000_000);
    }

    /// Every format goes through the same loop; one test per format would be
    /// three copies of the same assertion.
    #[tokio::test]
    async fn every_format_round_trips_through_the_loop() {
        for format in [ExportFormat::MaildirPlus, ExportFormat::EmlZip] {
            let corpus = corpus();
            let nest = Arc::new(FakeNest::new(&corpus));
            let m = machine_over(nest.clone());
            m.hydrate().await.unwrap();
            m.dispatch(MailExportAction::SelectFormat { format })
                .await
                .unwrap();
            m.dispatch(MailExportAction::Start).await.unwrap();
            m.run_export().await.expect("the run completes");
            assert_eq!(
                open_downloaded(&nest, format),
                expected_blob(format, &corpus),
                "{format:?}"
            );
        }
    }

    /// § An unopenable record fails the session. A record sealed to a key this
    /// account never held is NOT skipped: the session errors naming it, the
    /// partial blob is cancelled away, and nothing is finalized — because an
    /// archive quietly missing a message is the one outcome worse than none.
    #[tokio::test]
    async fn an_unopenable_record_fails_the_session_instead_of_being_skipped() {
        let foreign = [0x99u8; 32];
        let mut bad = stored(2, 1_700_000_001, "foreign");
        bad.sealed = sealed_to(&foreign, &bad.plain);
        let corpus = vec![(
            "INBOX",
            vec![
                stored(1, 1_700_000_000, "fine"),
                bad,
                stored(3, 1_700_000_002, "after"),
            ],
        )];
        let nest = Arc::new(FakeNest::new(&corpus));
        let m = machine_over(nest.clone());
        walk_to_confirm(&m).await;
        m.dispatch(MailExportAction::Start).await.unwrap();

        let err = m.run_export().await.expect_err("the session must fail");
        assert!(err.to_string().contains("uid 2"), "{err}");
        let snap = m.snapshot();
        assert_eq!(snap.session_state, Some(ExportSessionState::Errored));
        assert!(
            snap.error_log
                .iter()
                .any(|l| l.contains("INBOX") && l.contains("uid 2")),
            "{:?}",
            snap.error_log
        );
        assert_eq!(snap.skipped_count, 0, "nothing is ever counted as skipped");
        let st = nest.state.lock().unwrap();
        assert!(
            !st.cancelled,
            "the failure is recorded as one, not disguised as the user's cancel"
        );
        assert!(
            st.failed.as_deref().is_some_and(|r| r.contains("uid 2")),
            "the nest row carries the reason: {:?}",
            st.failed
        );
        assert!(st.blob.is_empty(), "the partial blob is disposed of");
        assert_eq!(
            st.session.as_ref().map(|s| s.state),
            Some(ExportSessionState::Errored),
            "a second client's listing reads `errored`, not `cancelled`"
        );
        assert!(!st.finalized, "no archive is produced");
    }

    /// A Pause arriving mid-run stops at a page boundary and keeps the live
    /// zstd stream; a Resume in the same machine continues it, and the result
    /// is still the exact archive. This is the warm half of § Resume — the only
    /// half the one-stream container allows without a nest-side reset.
    #[tokio::test]
    async fn a_mid_run_pause_keeps_the_stream_and_resume_finishes_the_same_archive() {
        let corpus = corpus();
        let mut fake = FakeNest::new(&corpus);
        fake.pause_after_fetches = Some(1);
        let nest = Arc::new(fake);
        let m = machine_over(nest.clone());
        walk_to_confirm(&m).await;
        m.dispatch(MailExportAction::Start).await.unwrap();

        m.run_export().await.expect("a pause is not an error");
        assert_eq!(m.snapshot().session_state, Some(ExportSessionState::Paused));
        assert!(!nest.state.lock().unwrap().finalized);

        m.dispatch(MailExportAction::Resume).await.unwrap();
        m.run_export().await.expect("the resumed run completes");
        assert_eq!(m.snapshot().step, ExportStep::Done);
        assert_eq!(
            open_downloaded(&nest, ExportFormat::Mbox),
            expected_blob(ExportFormat::Mbox, &corpus),
        );
    }

    /// A Cancel landing while a page is in flight disposes of the session on
    /// the nest at once, so the loop's next upload is refused. That refusal is
    /// the cancel's own consequence, not a failure: the user is back on a clean
    /// Format step with no error painted over it and no second cancel reported.
    #[tokio::test]
    async fn a_mid_page_cancel_ends_the_run_quietly_on_a_clean_wizard() {
        let mut fake = FakeNest::new(&corpus());
        fake.cancel_after_fetches = Some(1);
        let nest = Arc::new(fake);
        let m = machine_over(nest.clone());
        walk_to_confirm(&m).await;
        m.dispatch(MailExportAction::Start).await.unwrap();

        m.run_export().await.expect("a cancel is not an error");
        let snap = m.snapshot();
        assert_eq!(snap.step, ExportStep::Format);
        assert_eq!(snap.session_state, None);
        assert!(snap.error.is_none(), "{:?}", snap.error);
        assert!(snap.error_log.is_empty(), "{:?}", snap.error_log);
        assert!(
            m.run.lock().unwrap().is_none(),
            "the run and its key are gone"
        );
        let st = nest.state.lock().unwrap();
        assert!(st.cancelled);
        assert!(!st.finalized);
    }

    // ── § Resume — the cold resume ─────────────────────────────────────

    /// Drive `corpus` on a first machine until its first pause, then abandon
    /// that machine — the app was closed mid-export. Returns the nest, which
    /// now holds a session with a partial blob of the abandoned stream; the
    /// row is forced back to `running`, because nobody is left to have paused
    /// a real one.
    async fn an_export_abandoned_mid_run(corpus: &[(&str, Vec<Stored>)]) -> Arc<FakeNest> {
        let mut fake = FakeNest::new(corpus);
        // The fourth fetch is `Sent`'s only page (after `Archive`'s one and
        // INBOX's two). An mbox file is emitted when its mailbox closes and
        // reaches the encoder when the NEXT entry starts, so this is the first
        // page after which `Archive`'s bulk has actually been uploaded — and
        // the run still owes the archive its tail, so it is genuinely mid-run.
        fake.pause_after_fetches = Some(4);
        let nest = Arc::new(fake);
        let first = machine_over(nest.clone());
        walk_to_confirm(&first).await;
        first.dispatch(MailExportAction::Start).await.unwrap();
        first.run_export().await.expect("pauses after one page");
        drop(first);
        let mut st = nest.state.lock().unwrap();
        assert!(
            !st.blob.is_empty(),
            "the abandoned stream left frames behind"
        );
        st.session.as_mut().unwrap().state = ExportSessionState::Running;
        drop(st);
        nest
    }

    /// [`corpus`] with one message large and incompressible enough that the
    /// zstd encoder emits blocks mid-run. The small fixtures all fit inside the
    /// encoder's first block, so nothing reaches the nest before `finish` — and
    /// an abandoned export that uploaded nothing would prove nothing about
    /// stale frames.
    fn bulky_corpus() -> Vec<(&'static str, Vec<Stored>)> {
        let mut noise = String::with_capacity(400_000);
        let mut x: u32 = 0x9e37_79b9;
        while noise.len() < 400_000 {
            for _ in 0..72 {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                noise.push(char::from(b'!' + ((x >> 24) % 90) as u8));
            }
            noise.push_str("\r\n");
        }
        let plain = format!(
            "From: sender@example.com\r\nMessage-ID: <bulk@example.com>\r\n\
             Subject: bulk\r\n\r\n{noise}"
        )
        .into_bytes();
        let mut corpus = corpus();
        corpus[1].1.push(Stored {
            uid: 4,
            internal_date: 1_600_000_500,
            stored_at: 0,
            flags: Vec::new(),
            sealed: sealed_to(&MSEK, &plain),
            plain,
        });
        corpus
    }

    /// The flow this section makes true: an app that finds an export it did
    /// not start presses Resume, the stream restarts on the SAME session under
    /// a new generation, and it completes — to byte-for-byte the archive a
    /// one-shot serialization produces, opened under the key the restart
    /// minted, with not one frame of the abandoned stream in the blob.
    #[tokio::test]
    async fn a_cold_resume_restarts_the_stream_on_the_same_session_and_completes() {
        let corpus = bulky_corpus();
        let nest = an_export_abandoned_mid_run(&corpus).await;
        let abandoned_key = nest.state.lock().unwrap().wrapped_key.clone();

        let second = machine_over(nest.clone());
        second.hydrate().await.unwrap();
        assert_eq!(second.snapshot().step, ExportStep::Progress);
        second.dispatch(MailExportAction::Resume).await.unwrap();
        {
            let st = nest.state.lock().unwrap();
            assert_eq!(st.generation, 1, "a new stream generation, same session");
            assert!(
                st.blob.is_empty(),
                "the new stream starts from an empty blob"
            );
            assert_ne!(st.wrapped_key, abandoned_key, "one key per generation");
        }
        assert_eq!(second.snapshot().exported_count, 0, "counters restart too");

        second
            .run_export()
            .await
            .expect("the restarted run completes");
        let snap = second.snapshot();
        assert_eq!(snap.step, ExportStep::Done);
        assert_eq!(snap.exported_count, 6);
        // `open_downloaded` unwraps the row's CURRENT wrapped key and demands
        // every frame open in index order through the terminator — a single
        // stale frame, or a frame under the old key, fails it.
        assert_eq!(
            open_downloaded(&nest, ExportFormat::Mbox),
            expected_blob(ExportFormat::Mbox, &corpus),
        );
    }

    /// The scope a cold resume exports is the ROW's, not whatever this app's
    /// own wizard happens to have selected: the second machine never touched
    /// the scope step, whose defaults differ from what the first one chose.
    #[tokio::test]
    async fn a_cold_resume_exports_the_scope_the_session_was_started_with() {
        let corpus = corpus();
        let mut fake = FakeNest::new(&corpus);
        fake.pause_after_fetches = Some(1);
        let nest = Arc::new(fake);
        let first = machine_over(nest.clone());
        first.hydrate().await.unwrap();
        first
            .dispatch(MailExportAction::ToggleMailbox {
                mailbox: "Sent".into(),
            })
            .await
            .unwrap();
        first.dispatch(MailExportAction::Next).await.unwrap();
        first.dispatch(MailExportAction::Next).await.unwrap();
        first.dispatch(MailExportAction::Start).await.unwrap();
        first.run_export().await.unwrap();
        drop(first);

        let second = machine_over(nest.clone());
        second.hydrate().await.unwrap();
        second.dispatch(MailExportAction::Resume).await.unwrap();
        second.run_export().await.unwrap();
        let without_sent: Vec<(&str, Vec<Stored>)> = corpus
            .iter()
            .filter(|(name, _)| *name != "Sent")
            .cloned()
            .collect();
        assert_eq!(
            open_downloaded(&nest, ExportFormat::Mbox),
            expected_blob(ExportFormat::Mbox, &without_sent),
        );
    }

    /// The other half of § Resume: the device that WAS driving learns it has
    /// been restarted over, drops its stream, and touches nothing — the session
    /// stays alive for the device now driving it.
    #[tokio::test]
    async fn a_driver_restarted_over_drops_its_run_and_never_cancels_the_new_stream() {
        let mut fake = FakeNest::new(&corpus());
        fake.restart_after_fetches = Some(1);
        let nest = Arc::new(fake);
        let m = machine_over(nest.clone());
        walk_to_confirm(&m).await;
        m.dispatch(MailExportAction::Start).await.unwrap();

        let err = m.run_export().await.expect_err("the run is superseded");
        assert!(err.to_string().contains("restarted from another"), "{err}");
        let st = nest.state.lock().unwrap();
        assert!(!st.cancelled, "the superseded driver must not cancel");
        assert!(
            st.blob.is_empty(),
            "and not one of its frames reached the new stream"
        );
        assert_eq!(
            st.session.as_ref().map(|s| s.state),
            Some(ExportSessionState::Running)
        );
        drop(st);
        assert!(
            m.run.lock().unwrap().is_none(),
            "its stream and key are gone"
        );
        let snap = m.snapshot();
        assert_eq!(snap.session_state, Some(ExportSessionState::Running));
        assert!(snap.mailbox_progress.is_empty());
    }

    /// A failure that is this device's alone — a dropped fetch — lands AFTER
    /// another device restarted the export. The failure path's disposal is
    /// conditioned on the run's own generation, so it disposes of nothing —
    /// and records nothing either: the export is healthy on the other device,
    /// and an `errored` row would be a lie about a running stream.
    #[tokio::test]
    async fn a_superseded_drivers_failure_path_does_not_dispose_of_the_new_stream() {
        let mut fake = FakeNest::new(&corpus());
        fake.restart_after_fetches = Some(1);
        fake.fail_fetch = Some(1);
        let nest = Arc::new(fake);
        let m = machine_over(nest.clone());
        walk_to_confirm(&m).await;
        m.dispatch(MailExportAction::Start).await.unwrap();

        let err = m.run_export().await.expect_err("the fetch failed");
        assert!(err.to_string().contains("restarted from another"), "{err}");
        let st = nest.state.lock().unwrap();
        assert!(!st.cancelled);
        assert!(
            st.failed.is_none(),
            "the superseded driver must not mark the new stream errored"
        );
        drop(st);
        assert_ne!(
            m.snapshot().session_state,
            Some(ExportSessionState::Errored),
            "the export is healthy — on the other device"
        );
    }

    /// A parked (paused) stream whose session another device restarted: the
    /// warm Resume is refused as superseded even though the session already
    /// reads `running`, which is exactly what a warm resume asks for.
    #[tokio::test]
    async fn a_parked_stream_restarted_over_is_not_resumed_into_the_new_one() {
        let mut fake = FakeNest::new(&corpus());
        fake.pause_after_fetches = Some(1);
        let nest = Arc::new(fake);
        let m = machine_over(nest.clone());
        walk_to_confirm(&m).await;
        m.dispatch(MailExportAction::Start).await.unwrap();
        m.run_export().await.unwrap();
        nest.restart(b"the other device's key".to_vec(), 5).unwrap();

        let err = m
            .dispatch(MailExportAction::Resume)
            .await
            .expect_err("superseded");
        assert!(err.to_string().contains("restarted from another"), "{err}");
        assert!(m.run.lock().unwrap().is_none());
        assert_eq!(
            nest.state.lock().unwrap().generation,
            1,
            "no second restart"
        );
    }

    /// Without a Resume there is still no stream here to drive, and a `running`
    /// export this app does not drive must not be paused from it: the nest
    /// refuses uploads to a paused session, so the device that IS driving would
    /// lose the whole export.
    #[tokio::test]
    async fn a_cold_session_is_neither_driven_nor_paused_from_here() {
        let nest = Arc::new(FakeNest::names(&["INBOX"]));
        nest.state.lock().unwrap().session = Some(view(ExportSessionState::Running, 3));
        let m = machine_over(nest.clone());
        m.hydrate().await.unwrap();
        assert_eq!(m.snapshot().step, ExportStep::Progress);
        let err = m.run_export().await.expect_err("no live stream to drive");
        assert!(err.to_string().contains("press Resume"), "{err}");
        assert_eq!(nest.state.lock().unwrap().fetches, 0, "nothing was fetched");

        let err = m
            .dispatch(MailExportAction::Pause)
            .await
            .expect_err("a cold pause is refused");
        assert!(err.to_string().contains("not running in this app"), "{err}");
        assert_eq!(
            nest.state.lock().unwrap().session.as_ref().map(|s| s.state),
            Some(ExportSessionState::Running),
            "the nest was never asked"
        );
    }

    // ── § UX shape step 2 — the date range ─────────────────────────────

    /// The corpus messages whose INTERNALDATE the range admits — what a ranged
    /// archive must be a one-shot serialization of.
    fn within(
        corpus: &[(&'static str, Vec<Stored>)],
        since: i64,
        until_exclusive: i64,
    ) -> Vec<(&'static str, Vec<Stored>)> {
        corpus
            .iter()
            .map(|(name, recs)| {
                (
                    *name,
                    recs.iter()
                        .filter(|r| r.internal_date >= since && r.internal_date < until_exclusive)
                        .cloned()
                        .collect(),
                )
            })
            .collect()
    }

    async fn set_range(m: &MailExportMachine, from: &str, to: &str) {
        m.dispatch(MailExportAction::SetDateFrom { value: from.into() })
            .await
            .unwrap();
        m.dispatch(MailExportAction::SetDateTo { value: to.into() })
            .await
            .unwrap();
    }

    // 2023-11-14 is the UTC day holding 1_700_000_000..=1_700_000_100 — the
    // corpus's three "recent" messages; `archived` (2020) and `imported`
    // (1998) fall outside it.
    const DAY_START: i64 = 1_699_920_000;
    const DAY_END_EXCLUSIVE: i64 = DAY_START + 86_400;

    #[tokio::test]
    async fn a_date_range_exports_exactly_the_messages_inside_it() {
        let corpus = corpus();
        let nest = Arc::new(FakeNest::new(&corpus));
        let m = machine_over(nest.clone());
        m.hydrate().await.unwrap();
        set_range(&m, "2023-11-14", "2023-11-14").await;
        m.dispatch(MailExportAction::Next).await.unwrap();
        m.dispatch(MailExportAction::Next).await.unwrap();
        m.dispatch(MailExportAction::Start).await.unwrap();
        m.run_export().await.expect("the ranged run completes");

        let expected = within(&corpus, DAY_START, DAY_END_EXCLUSIVE);
        assert_eq!(
            expected.iter().map(|(_, r)| r.len()).sum::<usize>(),
            3,
            "fixture sanity: hello, gap and sent are the in-range three"
        );
        assert_eq!(
            open_downloaded(&nest, ExportFormat::Mbox),
            expected_blob(ExportFormat::Mbox, &expected),
            "byte-for-byte a one-shot serialization of the in-range subset"
        );
        let snap = m.snapshot();
        assert_eq!(snap.exported_count, 3);
        assert_eq!(
            snap.skipped_count, 0,
            "an out-of-range message is outside the scope, not a skip"
        );
        // The listing counted whole mailboxes (5); at completion nothing may
        // still overstate — neither the nest's total nor any mailbox's row.
        assert_eq!(snap.total_count, 3);
        for row in &snap.mailbox_progress {
            assert_eq!(row.exported, row.total, "{row:?}");
        }
    }

    /// Both ends are inclusive of their whole UTC day: a message in the last
    /// second of the `until` day is in, one second later is out; the first
    /// second of the `since` day is in, one second earlier is out.
    #[test]
    fn a_bare_date_names_its_whole_utc_day_inclusive_at_both_ends() {
        let range = ExportDateRange::from_scope(&ExportScope {
            date_from: "2023-11-14".into(),
            date_to: "2023-11-14".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(!range.admits(DAY_START - 1));
        assert!(range.admits(DAY_START));
        assert!(range.admits(DAY_END_EXCLUSIVE - 1));
        assert!(!range.admits(DAY_END_EXCLUSIVE));

        let open = ExportDateRange::from_scope(&ExportScope::default()).unwrap();
        assert!(!open.is_bounded());
        assert!(open.admits(i64::MIN) && open.admits(i64::MAX));

        let since_only = ExportDateRange::from_scope(&ExportScope {
            date_from: "2023-11-14".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(since_only.admits(i64::MAX) && !since_only.admits(DAY_START - 1));
    }

    /// A range the machine cannot read refuses the Start — before any session
    /// exists — instead of quietly exporting everything in its place.
    #[tokio::test]
    async fn a_malformed_or_inverted_range_refuses_start_before_opening_a_session() {
        for (from, to, needle) in [
            ("14/11/2023", "", "YYYY-MM-DD"),
            ("", "2023-02-30", "YYYY-MM-DD"),
            ("2023-11-15", "2023-11-14", "after its until"),
        ] {
            let nest = Arc::new(FakeNest::new(&corpus()));
            let m = machine_over(nest.clone());
            m.hydrate().await.unwrap();
            set_range(&m, from, to).await;
            let err = m
                .dispatch(MailExportAction::Start)
                .await
                .expect_err("a bad range refuses");
            assert!(err.to_string().contains(needle), "{from:?}..{to:?}: {err}");
            assert!(
                nest.state.lock().unwrap().session.is_none(),
                "no session may be opened for a range nobody can honour"
            );
        }
    }

    /// A cold resume is the SAME export: the range comes from the row's scope,
    /// not from the restarting app's own (untouched, unbounded) wizard.
    #[tokio::test]
    async fn a_cold_resume_keeps_the_sessions_date_range() {
        let corpus = corpus();
        let mut fake = FakeNest::new(&corpus);
        fake.pause_after_fetches = Some(1);
        let nest = Arc::new(fake);
        let first = machine_over(nest.clone());
        first.hydrate().await.unwrap();
        set_range(&first, "2023-11-14", "").await;
        first.dispatch(MailExportAction::Next).await.unwrap();
        first.dispatch(MailExportAction::Next).await.unwrap();
        first.dispatch(MailExportAction::Start).await.unwrap();
        first.run_export().await.unwrap();
        drop(first);

        let second = machine_over(nest.clone());
        second.hydrate().await.unwrap();
        second.dispatch(MailExportAction::Resume).await.unwrap();
        second.run_export().await.unwrap();
        assert_eq!(
            open_downloaded(&nest, ExportFormat::Mbox),
            expected_blob(ExportFormat::Mbox, &within(&corpus, DAY_START, i64::MAX)),
        );
    }

    /// A machine with no key custody cannot seal an export for anyone, so
    /// `Start` refuses before opening a session — never a session whose blob
    /// nothing can open.
    #[tokio::test]
    async fn start_without_key_custody_refuses_before_opening_a_session() {
        let nest = Arc::new(FakeNest::names(&["INBOX"]));
        let m = MailExportMachine::without_key_custody(nest.clone());
        walk_to_confirm(&m).await;
        assert!(m.dispatch(MailExportAction::Start).await.is_err());
        assert!(m.snapshot().error.is_some());
        assert!(nest.state.lock().unwrap().session.is_none());
    }

    // ── the wizard chrome ──────────────────────────────────────────────

    #[tokio::test]
    async fn hydrate_loads_mailboxes_with_default_selection() {
        let m = machine(&["INBOX", "Sent", "Trash", "Junk", "Work"]);
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.step, ExportStep::Format);
        assert_eq!(snap.format, ExportFormat::Mbox);
        let sel: Vec<_> = snap
            .mailboxes
            .iter()
            .map(|x| (x.name.as_str(), x.selected))
            .collect();
        // Raw-byte order — the listing's, which is the export's.
        assert_eq!(
            sel,
            vec![
                ("INBOX", true),
                ("Junk", false),
                ("Sent", true),
                ("Trash", false),
                ("Work", true),
            ]
        );
    }

    #[tokio::test]
    async fn wizard_navigates_format_scope_confirm_and_back() {
        let m = machine(&["INBOX"]);
        m.hydrate().await.unwrap();
        assert_eq!(m.snapshot().step, ExportStep::Format);
        m.dispatch(MailExportAction::Next).await.unwrap();
        assert_eq!(m.snapshot().step, ExportStep::Scope);
        m.dispatch(MailExportAction::Next).await.unwrap();
        assert_eq!(m.snapshot().step, ExportStep::Confirm);
        // Next past Confirm is a no-op (Start drives forward).
        m.dispatch(MailExportAction::Next).await.unwrap();
        assert_eq!(m.snapshot().step, ExportStep::Confirm);
        m.dispatch(MailExportAction::Back).await.unwrap();
        assert_eq!(m.snapshot().step, ExportStep::Scope);
    }

    #[tokio::test]
    async fn select_format_and_toggle_mailbox() {
        let m = machine(&["INBOX", "Work"]);
        m.hydrate().await.unwrap();
        m.dispatch(MailExportAction::SelectFormat {
            format: ExportFormat::EmlZip,
        })
        .await
        .unwrap();
        assert_eq!(m.snapshot().format, ExportFormat::EmlZip);
        m.dispatch(MailExportAction::ToggleMailbox {
            mailbox: "Work".into(),
        })
        .await
        .unwrap();
        let work = m
            .snapshot()
            .mailboxes
            .into_iter()
            .find(|x| x.name == "Work")
            .unwrap();
        assert!(!work.selected);
    }

    /// A deselected mailbox is neither counted into the estimate nor walked.
    #[tokio::test]
    async fn start_sends_the_wrapped_key_and_the_selected_mailboxes_total() {
        let nest = Arc::new(FakeNest::new(&corpus()));
        let m = machine_over(nest.clone());
        m.hydrate().await.unwrap();
        m.dispatch(MailExportAction::ToggleMailbox {
            mailbox: "Archive".into(),
        })
        .await
        .unwrap();
        m.dispatch(MailExportAction::Start).await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.step, ExportStep::Progress);
        assert_eq!(snap.session_state, Some(ExportSessionState::Running));
        let names: Vec<_> = snap
            .mailbox_progress
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(names, vec!["INBOX", "Sent"]);
        let st = nest.state.lock().unwrap();
        assert_eq!(st.started_total, Some(4));
        let wrapped = st.wrapped_key.as_ref().expect("wrapped key sent");
        assert!(
            ExportSessionKeyBlob::from_canonical_bytes(wrapped).is_ok(),
            "the wire carries the ExportSessionKeyBlob, not a raw key"
        );
    }

    #[tokio::test]
    async fn pause_then_resume_transitions_session() {
        let m = machine(&["INBOX"]);
        walk_to_confirm(&m).await;
        m.dispatch(MailExportAction::Start).await.unwrap();
        m.dispatch(MailExportAction::Pause).await.unwrap();
        assert_eq!(m.snapshot().session_state, Some(ExportSessionState::Paused));
        m.dispatch(MailExportAction::Resume).await.unwrap();
        assert_eq!(
            m.snapshot().session_state,
            Some(ExportSessionState::Running)
        );
    }

    #[tokio::test]
    async fn cancel_resets_to_format_and_keeps_mailboxes() {
        let m = machine(&["INBOX", "Work"]);
        walk_to_confirm(&m).await;
        m.dispatch(MailExportAction::Start).await.unwrap();
        assert_eq!(m.snapshot().step, ExportStep::Progress);
        m.dispatch(MailExportAction::Cancel).await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.step, ExportStep::Format);
        assert_eq!(snap.session_state, None);
        assert_eq!(snap.mailboxes.len(), 2, "mailbox options preserved");
        assert!(
            m.run.lock().unwrap().is_none(),
            "the run and its key are dropped"
        );
    }

    #[tokio::test]
    async fn completed_session_resumes_to_done() {
        let nest = Arc::new(FakeNest::names(&["INBOX"]));
        {
            let mut done = view(ExportSessionState::Completed, 100);
            done.blob_bytes = Some(4096);
            done.download_url = "/api/v1/export/abcd".into();
            nest.state.lock().unwrap().session = Some(done);
        }
        let m = machine_over(nest);
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.step, ExportStep::Done);
        assert_eq!(snap.blob_bytes, Some(4096));
        assert_eq!(snap.download_url, "/api/v1/export/abcd");
    }

    /// An errored row's reason becomes the error log's one line, and a repeated
    /// refresh does not duplicate it.
    #[tokio::test]
    async fn an_errored_rows_reason_is_the_error_logs_single_line() {
        let m = machine(&["INBOX"]);
        let mut snap = MailExportSnapshot::empty();
        let mut errored = view(ExportSessionState::Errored, 3);
        errored.error_reason = "INBOX: uid 2 could not be opened".into();
        snap.apply_session(errored.clone());
        snap.apply_session(errored);
        assert_eq!(snap.error_log, vec!["INBOX: uid 2 could not be opened"]);
        drop(m);
    }

    // ── export format label ─────────────────────────────────────────────

    #[test]
    fn export_format_label_maps_every_variant() {
        for (format, key) in [
            (ExportFormat::Mbox, "mail_export.format_mbox"),
            (ExportFormat::MaildirPlus, "mail_export.format_maildir"),
            (ExportFormat::EmlZip, "mail_export.format_eml"),
        ] {
            assert_eq!(export_format_label(format).key, key, "{format:?}");
        }
    }
}
