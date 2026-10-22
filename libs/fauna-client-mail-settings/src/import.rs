//! Shared orchestration for the user-facing `mail-import` wizard (the
//! bring-your-foreign-IMAP-mailbox-into-Fauna surface): a five-screen wizard
//! (Source → Scope → Confirm → Progress → Done) that pulls mail from a
//! foreign IMAP server (Gmail / Outlook / iCloud / generic) into the user's
//! Fauna mailbox, client-driven, credentials never touching nest.
//!
//! Authority for behavior: `docs/goal/behavior/mailbox-migration.md` § UX
//! shape (the six wizard *steps* the doc names — Source picker / Connection
//! test / Scope / Confirmation / Progress / Done), § RPC surface, § Progress
//! lives nest-side, § Resume protocol, § Failure handling. Authority for
//! UX/IDs: `tests/e2e-unified/ui.yaml` `mail-import` page (ratified
//! 2026-08-27; no `mail-import-*` element set exists for the doc's "step 2"
//! — the connect button lives on the Source screen and its outcome either
//! advances straight to Scope or reports an error on the same screen — so
//! this module's [`ImportStep`] has five variants, not six; "connecting" is
//! the `status` axis, exactly like `mail-export.md`'s Format/Loading split).
//!
//! Mirrors `export.rs` closely (same Snapshot/Action/Machine/dispatch/hydrate
//! shape, same client-side-steps-first / durable-commit-at-Start pattern) —
//! see that module's docs for the shared rationale. tui is the lead app; the
//! other six lift this shape (priority #2/#4).
//!
//! # Two seams, not one
//!
//! Unlike every sibling machine in this crate, mail-import is **client-driven**
//! (`mailbox-migration.md` § Client-driven streaming model): the *client*
//! opens the IMAP session to the foreign source and pushes each fetched
//! message to nest, so there are two independent WS/IMAP peers, not one:
//!
//! - [`MailImportNest`] — the nest half. **Real, not stubbed**: the nest-side
//!   `import_sessions` surface has been built since 2026-07-08 (§
//!   Implementation status today), and `fauna_mail::imap_client::MailImportClient`
//!   already exists as the concrete `rpc_glue` implementation to wrap.
//! - [`ImportSourceNest`] — the foreign-server half: opens the source IMAP
//!   session and lists its mailboxes. Backed by
//!   `libs/fauna-mail/src/imap_client` (`ImapSession`) in the real `rpc_glue`
//!   impl, generic over the platform's own `ImapTransport`/`ImapClock` (native
//!   TCP+rustls vs web sans-io-over-relay, § Where the IMAP client runs) — this
//!   crate never depends on that native/wasm split directly, exactly like the
//!   `MailImportNest` split from `MailExportNest`'s nest-only precedent.
//!
//! # The fetch-drive loop — [`MailImportMachine::run_import`]
//!
//! Once a session is `Running`, `run_import` walks every selected mailbox in
//! order (source order — § Batching), one throttled [`ImportSourceNest::fetch_window`]
//! at a time, packs outcomes through [`fauna_mail::imap_client::BatchPacker`]
//! (never reimplementing `dedup_key`/`sender_domain` — that's `BatchPacker`'s
//! own `to_item`, called exactly once, here), and sends each unit via
//! [`MailImportNest::send_unit`]. Pausable/cancellable between windows (§
//! Wizard steps step 5): `Pause` drains the in-flight window to nest before
//! stopping (the resume cursor only ever advances past *sent* messages);
//! `Cancel` aborts without draining it. A per-message error-budget breach (§
//! Per-message error budget) calls [`MailImportNest::fail_session`] and
//! stops. Two conditions instead PAUSE the session on the loop's own account
//! ([`ImportPauseReason`], § Failure handling), keeping the cursor at the
//! first message not yet handed over so the person's Resume takes up from
//! it: the nest answering a message `quota_exceeded`, and a connection — to
//! the source or to the nest — that stays down through the 5 s / 30 s / 2 min
//! retries.
//!
//! # Resume — in the same app, and after a restart
//!
//! The per-mailbox [`MailboxCursor`]s this loop tracks live in
//! [`MailImportMachine`]'s own memory while the app runs, so a `Pause` →
//! `Resume` within one app instance continues from exactly where it left off,
//! on the source connection the seam kept. An app that finds a live session it
//! did not start — it was restarted, or is another of the person's devices —
//! adopts it cold on `hydrate` (§ Resume protocol steps 1–3): the source comes
//! back from the row's `source_descriptor`, the mailbox selection from its
//! `scope`, each cursor from [`ImportSessionView::cursors`], the since date
//! from `date_from` and the duplicates choice from `skip_dedup`, and
//! [`MailImportSnapshot::resume_needs_credentials`] is set. Its Resume then needs the password again — or the Microsoft sign-in
//! on the Outlook source — and opens the source with it before anything else
//! happens; without one it is refused with a sentence and reaches neither the
//! source nor the nest (§ Credential handling: the credential lived only in
//! the closed app's memory). A `running` row its app left behind (closed or
//! crashed without pausing) is resumed without a nest `resume_import_session`,
//! which refuses a running session. Each mailbox then continues from
//! `last_processed_source_uid + 1` (the real seam's enumeration filters to
//! `uid >= n`, § Resume protocol step 3), and step 4 is the loop's own
//! comparison of the cursor's UIDVALIDITY against the one `examine` reports:
//! a mismatch means the source rebuilt that mailbox, so it is passed over
//! with a warning ([`MailImportSnapshot::rebuilt_mailboxes`]), the session
//! pauses rather than completing, and
//! [`MailImportAction::RestartRebuiltMailboxes`] walks it again from its first
//! message. One difference remains between the two resumes: a message a full
//! mailbox turned away advances the row's cursor, so the cold resume passes it
//! by where the warm one offers it again.
//!
//! **A known display gap, not a data-loss one:** `imported_count` /
//! `skipped_count` / `errored_count` on the rendered snapshot are accumulated
//! **client-side** from each `send_unit` reply plus this loop's own
//! source-fetch failures, not re-read from nest's `import_sessions` row after
//! every call (no RPC returns that per-call). A resumed session's counts
//! (read via `list_sessions` on `hydrate`) reflect only nest-side outcomes —
//! source-fetch failures never reach nest at all (they never became an
//! `ImportMessageItem`) — so a live run's `errored_count` can run slightly
//! ahead of what a page reload shows. No message is lost or double-counted
//! either way; only the displayed *tally* of source-side failures doesn't
//! survive a reload.
//!
//! # The Outlook source signs in with Microsoft
//!
//! Ruled 2026-10-09 (mailbox-migration.md § Microsoft sign-in — the
//! device-code grant): Microsoft has refused a password from mail apps since
//! 2024-09-16, so Outlook is a preset like Gmail and iCloud with one field —
//! the account's address, the XOAUTH2 user — and the OAuth 2.0 device-code
//! grant run by shared Rust ([`fauna_mail::imap_client::oauth_device`])
//! against the Microsoft identity platform, entirely on the client: never the
//! Bluesky bridge's pattern, which terminates on the nest.
//! [`MailImportAction::BeginMicrosoftSignIn`] asks for a code and puts it on
//! [`MailImportSnapshot::microsoft_signin`];
//! [`MailImportMachine::run_microsoft_sign_in`] — spawned by the app glue
//! exactly as `run_import` is — polls until the person finishes, and a grant
//! drives `Connect` by itself with `AUTHENTICATE XOAUTH2`. The tokens live in
//! this machine's private state only — never in the snapshot, never in a
//! request to the nest — and are zeroized at Done, Cancel, an error, and
//! with the machine (which is how leaving the wizard ends them). A
//! reconnect — a Resume, or a retry after a dropped source connection —
//! refreshes the access token once when it has under five minutes left; a
//! refused refresh ends the session `errored`. Until the project's
//! application ID is filled ([`fauna_mail::imap_client::MICROSOFT`]) the
//! sign-in is unavailable and the button is inert with a sentence. On a
//! session adopted after a restart the grant connects nothing by itself: the
//! token waits for the person's Resume (module docs, *Resume*).

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_client_config::MailImportSourceStore;
use fauna_core::localized::LocalizedText;
use fauna_core::mail_import_source::RememberedImportSource;
use fauna_core::secret::SecretString;
use fauna_mail::imap_client::oauth_device::{
    self, DeviceFlow, FlowOutcome, MicrosoftRegistration, OAuthClient, OAuthHttpResponse,
    TokenGrant,
};
use fauna_mail::imap_client::{
    BatchPacker, FetchOutcome, ImapClock, ImportUnit, MailboxEstimate, MailboxStatus, SpecialUse,
    UidEntry,
};
use fauna_protocol::MaybeSendSync;
use fauna_protocol::bridge_routing::{
    ClientImportOutcome, ClientImportOutcomeKind, IMPORT_SKIP_QUOTA_EXCEEDED, ImportMessageItem,
    RecordImportOutcomesReply,
};
use serde::{Deserialize, Serialize};
use std::ops::ControlFlow;

use crate::error::{DispatchError, NestError};

/// Step 1 (`mail-import-source-picker`) — which foreign provider the user is
/// importing from. Each non-`Generic` variant pre-fills the connection
/// fields with the provider's well-known server (§ Wizard steps step 1).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ImportSourceKind {
    Gmail,
    Outlook,
    ICloud,
    Generic,
}

/// Canonical label for an [`ImportSourceKind`], returned as [`LocalizedText`]
/// so each app resolves it through its own i18n runtime — the
/// [`export_format_label`](crate::export::export_format_label) precedent,
/// generalized to the `mail-import-source-picker` radio choice so a seventh
/// app cannot hand-roll a sixth copy of the same four-arm map.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn import_source_kind_label(kind: ImportSourceKind) -> LocalizedText {
    match kind {
        ImportSourceKind::Gmail => LocalizedText::key("mail_import.source_gmail"),
        ImportSourceKind::Outlook => LocalizedText::key("mail_import.source_outlook"),
        ImportSourceKind::ICloud => LocalizedText::key("mail_import.source_icloud"),
        ImportSourceKind::Generic => LocalizedText::key("mail_import.source_generic"),
    }
}

/// The `mail-import-source-picker`'s options, in painted order. tui's
/// `settings::mail_import::SOURCE_KINDS` and linux's own same-named const
/// each hand-copied this exact array — collapsed here, the `LINK_MODES`/
/// `TLS_MODES` precedent.
pub const SOURCE_KINDS: [ImportSourceKind; 4] = [
    ImportSourceKind::Gmail,
    ImportSourceKind::Outlook,
    ImportSourceKind::ICloud,
    ImportSourceKind::Generic,
];

/// The preset connection triple for a non-`Generic` provider — all three use
/// implicit TLS on 993 (§ Wizard steps step 1). `Generic` has no preset; the
/// user fills `host`/`port`/`tls_mode` themselves.
fn provider_preset(kind: ImportSourceKind) -> Option<(&'static str, u16)> {
    match kind {
        ImportSourceKind::Gmail => Some(("imap.gmail.com", 993)),
        ImportSourceKind::Outlook => Some(("outlook.office365.com", 993)),
        ImportSourceKind::ICloud => Some(("imap.mail.me.com", 993)),
        ImportSourceKind::Generic => None,
    }
}

/// Step 1 (Generic only) — `mail-import-source-tls-mode`. § Wizard steps: no
/// plaintext variant exists — the source password crosses this connection.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ImportTlsMode {
    /// TLS from the first byte — the 993 default, and what every provider
    /// preset uses.
    Implicit,
    /// Negotiated `STARTTLS` over a plaintext 143 connection.
    StartTls,
}

/// Canonical label for an [`ImportTlsMode`] — the `mail-import-source-tls-mode`
/// picker's two options, resolved the same [`import_source_kind_label`] way.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn import_tls_mode_label(mode: ImportTlsMode) -> LocalizedText {
    match mode {
        ImportTlsMode::Implicit => LocalizedText::key("mail_import.tls_implicit"),
        ImportTlsMode::StartTls => LocalizedText::key("mail_import.tls_starttls"),
    }
}

/// The `mail-import-source-tls-mode` picker's options, in painted order.
/// tui's `settings::mail_import::TLS_MODES` and linux's own same-named const
/// each hand-copied this exact array — collapsed here since both are
/// Rust-native and already depend on this crate (the `LINK_MODES` precedent,
/// `fauna_client_bridges::labels`).
pub const TLS_MODES: [ImportTlsMode; 2] = [ImportTlsMode::Implicit, ImportTlsMode::StartTls];

/// Which wizard screen is showing. Five variants, not the doc's six wizard
/// *steps* — see the module docs' note on why "Connection test" has no
/// screen of its own.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ImportStep {
    /// Step 1 — the `mail-import-source-*` fields + `mail-import-connect-button`.
    Source,
    /// Step 3 — the `mail-import-scope-*` controls.
    Scope,
    /// Step 4 — `mail-import-confirm-summary` + `mail-import-start-button`.
    Confirm,
    /// Step 5 — `mail-import-progress-*` + the mailbox list + error log.
    Progress,
    /// Step 6 — `mail-import-done-summary` + the two deep-link buttons.
    Done,
}

/// The `import_sessions.state` the progress/done screens render
/// (`mailbox-migration.md` § Progress lives nest-side). Same five states as
/// `mail-export.md`'s `export_sessions` — same concept, same names
/// (priority #3).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ImportSessionState {
    Running,
    Paused,
    Errored,
    Completed,
    Cancelled,
}

/// One source mailbox the scope step (`mail-import-scope-mailboxes`) offers,
/// with its selection state. Only `LIST`-selectable mailboxes are ever
/// offered here — a `\Noselect` container (Gmail's bare `[Gmail]`) holds no
/// messages and is filtered out by [`MailImportMachine::connect`], not shown
/// as an unselectable row (§ Wizard steps step 3 is about *content*
/// mailboxes, never about protocol-level containers).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SourceMailboxOption {
    pub name: String,
    pub selected: bool,
    /// How many of its messages the import would take — the scope step's
    /// since date applied — for the Confirm step's total
    /// (`mailbox-migration.md` § Wizard steps step 4). `0` until the
    /// Scope → Confirm `Next` has estimated this mailbox
    /// ([`ImportSourceNest::estimate_mailbox`]); `LIST` carries no count.
    pub message_count: u32,
    /// Those messages' total size in bytes, from the same estimate; `None`
    /// before one, or when the source left a message unsized or refused to
    /// be asked — the Confirm step then names no size at all.
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    #[serde(default)]
    pub byte_total: Option<u64>,
}

/// The selected mailboxes' summed estimate the Confirm step states —
/// [`MailImportSnapshot::selected_totals`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ImportTotals {
    pub mailboxes: u32,
    pub messages: u64,
    /// `None` when any selected mailbox has no size estimate.
    pub bytes: Option<u64>,
}

/// [`MailImportSnapshot::selected_totals`] as a free function — the door the
/// binding apps (UniFFI, wasm) reach the Confirm step's sum through, so none of
/// them re-sums `message_count` itself (`mailbox-migration.md` § Wizard steps
/// step 4).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn import_selected_totals(snapshot: MailImportSnapshot) -> ImportTotals {
    snapshot.selected_totals()
}

/// One source mailbox as `LIST` reports it, before selection state exists.
/// What [`ImportSourceNest::list_source_mailboxes`] returns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceMailboxView {
    pub name: String,
    /// `false` for a `\Noselect` container (e.g. Gmail's bare `[Gmail]`).
    pub selectable: bool,
    /// The role the source advertises for it (`\Trash`, `\Junk`, `\All`,
    /// `\Flagged`, Gmail's `\Important`, …), `None` when it advertises none —
    /// what § Wizard steps step 3 recognises the discard and duplicate
    /// mailboxes by, before their names.
    pub special_use: Option<SpecialUse>,
}

/// [`run_import`](MailImportMachine::run_import)'s resume position for one
/// mailbox (§ Resume protocol) — the machine's working copy of the wire's
/// `ImportMailboxCursor`, advanced in memory as the run hands messages over
/// and seeded from the session row when a fresh app adopts the session
/// (module docs, *Resume*). `None` (no cursor yet) is what a mailbox never
/// yet examined passes to [`ImportSourceNest::examine`] /
/// [`ImportSourceNest::fetch_window`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxCursor {
    pub last_processed_source_uid: u32,
    pub source_uid_validity: u32,
}

/// One mailbox's resume cursor as the session row records it
/// (`mailbox-migration.md` § Progress lives nest-side,
/// `last_processed_source_uid_per_mailbox` and its UIDVALIDITY twin) — what a
/// fresh app seeds [`MailboxCursor`]s from (§ Resume protocol step 3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ImportMailboxCursorView {
    pub mailbox: String,
    pub last_processed_source_uid: u32,
    pub source_uid_validity: u32,
}

/// A live/finished import session as the nest seam reports it (projects
/// `ImportSessionInfo`). The UI renders the counts and times; the scope, the
/// since date and the cursors are what a fresh app resumes the walk from
/// (module docs, *Resume*).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ImportSessionView {
    pub session_id: String,
    pub state: ImportSessionState,
    pub source_descriptor: String,
    pub total_count: u64,
    pub imported_count: u64,
    pub skipped_count: u64,
    pub errored_count: u64,
    /// Populated when `state == Errored` (§ Failure handling).
    pub error_reason: String,
    /// The source mailbox names recorded at `start_import_session`
    /// (`mailbox-migration.md` § Resume protocol) — what a resumed session
    /// (after a client restart) must re-`EXAMINE`. Empty for a session that
    /// recorded no scope.
    pub scope: Vec<String>,
    /// The scope step's "since" date recorded at `start_import_session`
    /// (§ Wizard steps step 3) — what a resumed walk re-applies so it takes
    /// the range the user asked for and not the whole mailbox. Empty means
    /// unbounded (the user's choice).
    pub date_from: String,
    /// The scope step's "Import duplicates anyway" choice recorded at
    /// `start_import_session` (§ Opt-out per session) — what a resumed walk
    /// sends as every message's `skip_dedup`, so a resume after a restart
    /// neither duplicates mail the user did not ask to nor drops the snapshot
    /// they did. `false` (dedup on) for a session that recorded none.
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    #[serde(default)]
    pub skip_dedup: bool,
    /// The row's per-mailbox resume cursors (§ Resume protocol step 3).
    /// Empty for a session that has handed nothing over yet.
    #[cfg_attr(feature = "uniffi", uniffi(default = []))]
    #[serde(default)]
    pub cursors: Vec<ImportMailboxCursorView>,
    /// The row's `started_at` and `last_progress_at`, epoch seconds — what
    /// [`crate::timing::transfer_timing`] reads for the progress step's
    /// elapsed time and estimate and the Done step's "time elapsed"
    /// (§ Wizard steps steps 5–6).
    #[cfg_attr(feature = "uniffi", uniffi(default = 0))]
    #[serde(default)]
    pub started_at: i64,
    #[cfg_attr(feature = "uniffi", uniffi(default = 0))]
    #[serde(default)]
    pub last_progress_at: i64,
}

/// The scope step's "since" date, resolved to the instant the drive loop
/// compares INTERNALDATEs against (`mailbox-migration.md` § Wizard steps
/// step 3; the date grammar is `mail-export.md` § UX shape step 2's, adopted
/// rather than minted afresh — hence the shared
/// [`fauna_core::caltime::days_from_ymd`] parser and the export's own
/// `ExportDateRange` as this type's shape).
///
/// **A bare date names a whole UTC day and the floor is that day's first
/// second**, so a message stamped at 00:00:00 on the named day is in. There is
/// no upper half: § Wizard steps step 3 offers a "since" and deliberately no
/// "until".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ImportDateFloor {
    /// Inclusive lower bound, epoch seconds. `None` = unbounded.
    since: Option<i64>,
}

impl ImportDateFloor {
    /// Resolve the wizard's date string. Empty is unbounded; anything else
    /// must be a strict `YYYY-MM-DD` naming a real day.
    ///
    /// ⚠ A malformed date is an ERROR, never "no floor": importing the whole
    /// source mailbox in place of a range nobody could read spends the user's
    /// quota on mail they explicitly excluded (§ Quota composition), which is
    /// the exact under-delivering control the field exists to prevent.
    fn parse(value: &str) -> Result<Self, DispatchError> {
        if value.is_empty() {
            return Ok(Self { since: None });
        }
        let day = fauna_core::caltime::days_from_ymd(value).ok_or_else(|| {
            DispatchError::InvalidState(
                "the import's since date must be a real date written YYYY-MM-DD".into(),
            )
        })?;
        Ok(Self {
            since: Some(day * 86_400),
        })
    }

    fn admits(&self, internal_date: i64) -> bool {
        self.since.is_none_or(|s| internal_date >= s)
    }
}

/// Why [`MailImportMachine::run_import`] paused the session on its own,
/// rather than at the person's Pause (`mailbox-migration.md` § Failure
/// handling). Either way the session is `paused` with its cursor kept, and
/// the existing Resume takes it up again.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ImportPauseReason {
    /// The nest answered a message `quota_exceeded` — the person's Fauna
    /// mailbox is full (§ Quota composition).
    MailboxFull,
    /// The source server or the nest stayed unreachable through every retry
    /// (§ Failure handling → *Source-side errors*, *Nest-side errors*).
    ConnectionLost,
    /// The source rebuilt a mailbox since the import began — its UIDVALIDITY
    /// no longer matches the cursor (§ Resume protocol step 4). The run took
    /// every other mailbox and stopped short of completing, so the restart
    /// offer ([`MailImportSnapshot::rebuilt_mailboxes`]) stays on the progress
    /// step.
    MailboxRebuilt,
}

/// Coarse machine status for spinner / disabled-control rendering.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ImportStatus {
    Idle,
    Loading,
    Working,
}

/// Where the Outlook source's Microsoft sign-in stands
/// (`mailbox-migration.md` § Microsoft sign-in — the device-code grant).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum MicrosoftSignInState {
    /// Asking Microsoft for a code.
    Requesting,
    /// The code is shown; waiting for the person to enter it in a browser.
    Waiting,
    /// Microsoft granted the token; connecting to the source with it.
    Connecting,
    /// The person declined the sign-in.
    Declined,
    /// The code ran out before the person finished.
    Expired,
    /// The organisation's administrator has to approve Fauna first.
    AdminConsentRequired,
    /// Anything else that ended the attempt; the reason went to the log.
    Failed,
}

/// The sign-in's visible half: the code to type and where. The tokens
/// themselves never appear here — they live in the machine's private state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MicrosoftSignInView {
    /// Empty until Microsoft has answered with a code.
    pub user_code: String,
    pub verification_uri: String,
    /// When the code dies, in epoch milliseconds (the [`RetryClock`]'s
    /// frame); `0` before a code exists.
    pub expires_at_ms: u64,
    pub state: MicrosoftSignInState,
}

/// One per-mailbox progress row (`mail-import-mailbox-progress-list-item`,
/// `mailbox-migration.md` § Wizard steps step 5) — the export twin's
/// `MailboxProgressView`, with the skip and failure counts the import's own
/// summary carries. Kept by the drive loop alone: the nest's session row
/// holds aggregate counters only, so these rows, like the resume cursors,
/// live for the wizard's process.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ImportMailboxProgress {
    /// The source mailbox (`…-list-item-name`).
    pub name: String,
    pub imported: u64,
    pub skipped: u64,
    pub errored: u64,
    /// The messages the Confirm step counted for this mailbox — its share of
    /// the session's `total_count`.
    pub total: u64,
}

/// A progress row's `…-list-item-progress` text — the one formatter every
/// app paints the row through (`mail_import.progress_row_counts_fmt`).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn import_mailbox_progress_text(row: ImportMailboxProgress) -> LocalizedText {
    LocalizedText::key_args(
        "mail_import.progress_row_counts_fmt",
        [
            ("imported", row.imported.to_string()),
            ("count", row.total.to_string()),
            ("skipped", row.skipped.to_string()),
            ("errored", row.errored.to_string()),
        ],
    )
}

/// Read-only snapshot the per-app UI renders for `mail-import`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailImportSnapshot {
    pub step: ImportStep,
    // ── source (step 1) ─────────────────────────────────────────────
    pub source_kind: ImportSourceKind,
    pub host: String,
    pub port: u16,
    pub tls_mode: ImportTlsMode,
    pub username: String,
    /// Client-memory-only (`mailbox-migration.md` § Credential handling) —
    /// cleared the moment [`MailImportMachine::connect`] succeeds, since
    /// nothing past the Source screen needs it again. Kept in the snapshot
    /// (not a side channel) only so the password field can render the
    /// user's in-progress typing and a failed-connect retry keeps it
    /// populated ("the user can retry from this screen without re-entering
    /// credentials", § Wizard steps step 2).
    pub password: SecretString,
    // ── scope (step 3) ──────────────────────────────────────────────
    pub mailboxes: Vec<SourceMailboxOption>,
    pub date_from: String,
    /// Bytes; default 50 MiB (§ Wizard steps step 3).
    pub max_size_bytes: u64,
    /// The advanced "Import duplicates anyway" option
    /// (`mail-import-scope-import-duplicates-toggle`, § Opt-out per session):
    /// off by default; on, every message is sent with `skip_dedup`, so mail
    /// the nest already holds is stored a second time. Recorded on the
    /// session row at Start and adopted from it by a wizard that finds the
    /// session (§ Resume protocol step 3).
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    #[serde(default)]
    pub import_duplicates: bool,
    // ── progress / done (steps 5–6; from the session) ──────────────
    pub session_state: Option<ImportSessionState>,
    pub imported_count: u64,
    pub skipped_count: u64,
    pub errored_count: u64,
    pub total_count: u64,
    /// Per-message skip/error lines (`mail-import-error-log`). Populated by
    /// `run_import` through `push_error_log`; empty until a run records one.
    pub error_log: Vec<String>,
    /// One row per mailbox the run takes, laid by `Start` and counted by
    /// `run_import` — see [`ImportMailboxProgress`]. Empty before a Start in
    /// this process.
    #[cfg_attr(feature = "uniffi", uniffi(default = []))]
    #[serde(default)]
    pub mailbox_progress: Vec<ImportMailboxProgress>,
    /// The session's `started_at` / `last_progress_at` (epoch seconds; `0`
    /// before a session exists) — read through [`Self::timing`].
    #[cfg_attr(feature = "uniffi", uniffi(default = 0))]
    #[serde(default)]
    pub started_at: i64,
    #[cfg_attr(feature = "uniffi", uniffi(default = 0))]
    #[serde(default)]
    pub last_progress_at: i64,
    pub status: ImportStatus,
    /// Last action's error, surfaced via `error-message`.
    pub error: Option<String>,
    /// Set when the drive loop paused the session itself; its sentence is
    /// also on [`Self::error`], so every app already paints it. Cleared by
    /// the next Resume.
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    #[serde(default)]
    pub pause_reason: Option<ImportPauseReason>,
    /// Source mailboxes the old provider rebuilt since the import began — the
    /// UIDVALIDITY `examine` reports no longer matches the cursor
    /// (`mailbox-migration.md` § Resume protocol step 4). The run passes each
    /// over with its warning in [`Self::error_log`] and pauses the session
    /// instead of completing it; every app paints `mail-import-restart-button`
    /// while this is non-empty, and
    /// [`MailImportAction::RestartRebuiltMailboxes`] takes them again from
    /// their first message. Lives for the wizard's process, like the cursors.
    #[cfg_attr(feature = "uniffi", uniffi(default = []))]
    #[serde(default)]
    pub rebuilt_mailboxes: Vec<String>,
    /// This app adopted a live session it did not start — it was restarted,
    /// or is another of the person's devices — so it holds no source
    /// credential: Resume first asks for the password again (Gmail and
    /// iCloud: the app password; Generic: the password, with the port and
    /// connection security beside it), or for the Microsoft sign-in on the
    /// Outlook source (§ Resume protocol step 2). Every app paints that field
    /// on the Progress step while this is set; a Resume that reaches the
    /// source clears it.
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    #[serde(default)]
    pub resume_needs_credentials: bool,
    /// Whether this platform can sign in with Microsoft at all: the project's
    /// application ID is filled and the platform has an HTTPS seam for it.
    /// `false` paints the Outlook button inert with
    /// `mail_import.microsoft_unavailable`.
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    #[serde(default)]
    pub microsoft_signin_available: bool,
    /// The Outlook source's sign-in, while one is under way or has just
    /// ended; `None` otherwise.
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    #[serde(default)]
    pub microsoft_signin: Option<MicrosoftSignInView>,
    /// Whether the account remembers a source's server, port, connection
    /// security and username (`mailbox-migration.md` § Credential handling)
    /// — the Source step paints `mail-import-source-forget-button` while it
    /// does, and [`MailImportAction::ForgetSource`] clears it.
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    #[serde(default)]
    pub source_remembered: bool,
}

/// § Wizard steps step 3's default mailbox selection: every mailbox starts
/// ticked except the ones holding discards (Trash, Junk/Spam) or duplicates
/// of mail another mailbox already holds (Gmail's All Mail, Important,
/// Starred). Recognised by the role the source advertises first — a
/// localized Gmail lists none of the English names — and by the well-known
/// names otherwise, Gmail's under either of its two folder prefixes. Never
/// hidden: the user ticks one to take it.
fn default_selected(name: &str, special_use: Option<SpecialUse>) -> bool {
    if let Some(role) = special_use {
        return !matches!(
            role,
            SpecialUse::Trash
                | SpecialUse::Junk
                | SpecialUse::All
                | SpecialUse::Flagged
                | SpecialUse::Important
        );
    }
    let lower = name.to_ascii_lowercase();
    if matches!(lower.as_str(), "trash" | "junk" | "spam") {
        return false;
    }
    let gmail_label = lower
        .strip_prefix("[gmail]/")
        .or_else(|| lower.strip_prefix("[google mail]/"));
    !matches!(
        gmail_label,
        Some("all mail" | "bin" | "trash" | "spam" | "important" | "starred")
    )
}

impl MailImportSnapshot {
    /// The page as it paints before `hydrate` has answered: step 1, the Gmail
    /// preset, nothing known about the source or the mailbox. `pub` for the
    /// `ArchiveImportSnapshot::empty` reason — an app's own tests fold a
    /// snapshot through their page without standing up a whole machine.
    pub fn empty() -> Self {
        Self {
            step: ImportStep::Source,
            source_kind: ImportSourceKind::Gmail,
            host: provider_preset(ImportSourceKind::Gmail)
                .expect("gmail has a preset")
                .0
                .to_string(),
            port: provider_preset(ImportSourceKind::Gmail)
                .expect("gmail has a preset")
                .1,
            tls_mode: ImportTlsMode::Implicit,
            username: String::new(),
            password: SecretString::default(),
            mailboxes: Vec::new(),
            date_from: String::new(),
            max_size_bytes: 50 * 1024 * 1024,
            import_duplicates: false,
            session_state: None,
            imported_count: 0,
            skipped_count: 0,
            errored_count: 0,
            total_count: 0,
            error_log: Vec::new(),
            mailbox_progress: Vec::new(),
            started_at: 0,
            last_progress_at: 0,
            status: ImportStatus::Idle,
            error: None,
            pause_reason: None,
            rebuilt_mailboxes: Vec::new(),
            resume_needs_credentials: false,
            microsoft_signin_available: false,
            microsoft_signin: None,
            source_remembered: false,
        }
    }

    /// The row `mailbox`'s outcomes count against — `None` for one outside
    /// the rows `Start` laid.
    fn mailbox_row(&mut self, mailbox: &str) -> Option<&mut ImportMailboxProgress> {
        self.mailbox_progress.iter_mut().find(|r| r.name == mailbox)
    }

    /// The progress step's elapsed time and estimate, and the Done step's
    /// time elapsed (§ Wizard steps steps 5–6), at the client's `now_secs`.
    pub fn timing(&self, now_secs: i64) -> crate::timing::TransferTiming {
        crate::timing::transfer_timing(
            self.started_at,
            self.last_progress_at,
            self.session_state == Some(ImportSessionState::Running),
            self.imported_count + self.skipped_count + self.errored_count,
            self.total_count,
            now_secs,
        )
    }

    /// The `(source_descriptor, total_count, scope, date_from, skip_dedup)` tuple
    /// `start_session` sends — descriptor never carries the password
    /// (`mailbox-migration.md` § Progress lives nest-side:
    /// `source_descriptor` is "provider + hostname + username — no
    /// password"); `scope` is every selected mailbox's name, recorded so a
    /// resumed session knows what to re-`EXAMINE` (§ Resume protocol); and
    /// `date_from` is the scope step's since date, recorded for the same
    /// reason one step on — the row is the only durable record of how much
    /// of those mailboxes the session was allowed to take — and `skip_dedup`,
    /// the duplicates choice, for the same reason (§ Opt-out per session).
    fn start_args(&self) -> (String, u64, Vec<String>, String, bool) {
        let descriptor = format!(
            "{}:{}@{}",
            source_kind_label(self.source_kind),
            self.username,
            self.host
        );
        let scope = self
            .mailboxes
            .iter()
            .filter(|m| m.selected)
            .map(|m| m.name.clone())
            .collect();
        let total = self.selected_totals().messages;
        (
            descriptor,
            total,
            scope,
            self.date_from.clone(),
            self.import_duplicates,
        )
    }

    /// What the Confirm step summarizes (`mailbox-migration.md` § Wizard
    /// steps step 4): the selected mailboxes, their messages and — when every
    /// one of them was sized — their bytes. The message total is also the
    /// `total_count` `Start` opens the session with.
    pub fn selected_totals(&self) -> ImportTotals {
        let selected = || self.mailboxes.iter().filter(|m| m.selected);
        ImportTotals {
            mailboxes: u32::try_from(selected().count()).unwrap_or(u32::MAX),
            messages: selected().map(|m| u64::from(m.message_count)).sum(),
            bytes: selected().map(|m| m.byte_total).sum(),
        }
    }

    /// Apply a session's live state to the progress/done fields + derive the
    /// step.
    fn apply_session(&mut self, s: ImportSessionView) {
        // A session found at hydrate (§ Resume protocol step 1) carries the
        // range it was started under, and this wizard — a fresh process —
        // holds none: adopt the row's, so a resumed walk filters on what the
        // user actually asked for. Only when ours is empty, though: an unbounded
        // session answers every call with an empty
        // `date_from`, and letting that blank a live wizard's own field would
        // turn a display into a lie about what this run is doing.
        if self.date_from.is_empty() {
            self.date_from = s.date_from.clone();
        }
        // The duplicates choice is the row's, whatever this wizard holds: it
        // was fixed at Start and nothing changes it after, so the row is what
        // every message of this session is sent under (§ Resume protocol
        // step 3) — a fresh wizard that kept its own `false` would quietly
        // drop the snapshot the user asked for.
        self.import_duplicates = s.skip_dedup;
        self.session_state = Some(s.state);
        // A session that has ended has nothing left to resume, so nothing to
        // ask a credential for.
        if !matches!(
            s.state,
            ImportSessionState::Running | ImportSessionState::Paused
        ) {
            self.resume_needs_credentials = false;
        }
        self.imported_count = s.imported_count;
        self.skipped_count = s.skipped_count;
        self.errored_count = s.errored_count;
        self.total_count = s.total_count;
        self.started_at = s.started_at;
        self.last_progress_at = s.last_progress_at;
        // § A failed import is a record …: the recorded reason is the error
        // log's LAST line — on a cold adoption its only one, since the
        // per-message lines live in the app that ran the import. Written only
        // when it is not already there, so a repeated refresh, or the run's
        // own `fail_and_report`, never writes it twice. `error-message` is the
        // run's own error, set by the run that failed; a record adopted cold
        // carries none.
        if s.state == ImportSessionState::Errored && !s.error_reason.is_empty() {
            self.note_failure_reason(s.error_reason);
        }
        self.step = match s.state {
            ImportSessionState::Completed => ImportStep::Done,
            // Running / paused / errored / cancelled all render the Progress
            // screen; § UX shape step 5 keeps the error log + counts visible
            // after Cancel rather than resetting the wizard — already-imported
            // messages are kept, so there is real state worth reviewing.
            _ => ImportStep::Progress,
        };
    }

    /// End the error log with a failed import's reason, once.
    fn note_failure_reason(&mut self, reason: String) {
        if self.error_log.last() != Some(&reason) {
            self.error_log.push(reason);
        }
    }

    /// Return the wizard to the Source step with no session: everything a
    /// session or a run wrote goes, while the source the person last named —
    /// its kind, server and username, never a password — stays filled in, so
    /// the next import starts from where the last one pointed.
    fn reset_to_source(&mut self) {
        let mut fresh = MailImportSnapshot::empty();
        fresh.source_kind = self.source_kind;
        fresh.host = std::mem::take(&mut self.host);
        fresh.port = self.port;
        fresh.tls_mode = self.tls_mode;
        fresh.username = std::mem::take(&mut self.username);
        fresh.microsoft_signin_available = self.microsoft_signin_available;
        fresh.source_remembered = self.source_remembered;
        *self = fresh;
    }
}

/// Which listed session a machine holding none adopts (`mailbox-migration.md`
/// § A failed import is a record the wizard shows until the user moves on):
/// the newest live one (`running` / `paused`); with none, the newest
/// `errored` one; a `completed` one never — its Done step has nothing to
/// collect — and a `cancelled` one never. The listing arrives newest first
/// (`list_import_sessions` orders by `started_at` descending), so "newest" is
/// the first match.
fn session_to_show(sessions: &[ImportSessionView]) -> Option<ImportSessionView> {
    sessions
        .iter()
        .find(|s| {
            matches!(
                s.state,
                ImportSessionState::Running | ImportSessionState::Paused
            )
        })
        .or_else(|| {
            sessions
                .iter()
                .find(|s| s.state == ImportSessionState::Errored)
        })
        .cloned()
}

/// `source_descriptor`'s provider label — lowercase, matching
/// `send.rs`'s own doctest example (`"gmail:user@example.com"`).
fn source_kind_label(kind: ImportSourceKind) -> &'static str {
    match kind {
        ImportSourceKind::Gmail => "gmail",
        ImportSourceKind::Outlook => "outlook",
        ImportSourceKind::ICloud => "icloud",
        ImportSourceKind::Generic => "imap",
    }
}

/// [`MailImportSnapshot::start_args`]'s descriptor read back —
/// `<provider>:<username>@<host>` → `(kind, username, host)`. The username is
/// usually an address itself, so the host is what follows the LAST `@`.
/// `None` for anything else (an empty descriptor — a seal this app could not
/// open — or one some other client wrote).
fn parse_source_descriptor(descriptor: &str) -> Option<(ImportSourceKind, String, String)> {
    let (label, rest) = descriptor.split_once(':')?;
    let kind = SOURCE_KINDS
        .iter()
        .copied()
        .find(|k| source_kind_label(*k) == label)?;
    let (username, host) = rest.rsplit_once('@')?;
    if username.is_empty() || host.is_empty() {
        return None;
    }
    Some((kind, username.to_string(), host.to_string()))
}

/// [`ImportTlsMode`]'s label in a [`RememberedImportSource`].
fn tls_mode_label(mode: ImportTlsMode) -> &'static str {
    match mode {
        ImportTlsMode::Implicit => "implicit",
        ImportTlsMode::StartTls => "starttls",
    }
}

/// A [`RememberedImportSource`] read back into the wizard's own types —
/// `None` for a record this build cannot read (a provider or connection
/// security it does not know, an empty host or username, port 0), which
/// prefills nothing rather than a guess.
fn parse_remembered(
    r: &RememberedImportSource,
) -> Option<(ImportSourceKind, ImportTlsMode, &RememberedImportSource)> {
    let kind = SOURCE_KINDS
        .iter()
        .copied()
        .find(|k| source_kind_label(*k) == r.provider)?;
    let tls = TLS_MODES
        .iter()
        .copied()
        .find(|m| tls_mode_label(*m) == r.tls_mode)?;
    if r.host.is_empty() || r.username.is_empty() || r.port == 0 {
        return None;
    }
    Some((kind, tls, r))
}

impl MailImportSnapshot {
    /// What a successful connection remembers (§ Credential handling): the
    /// source this snapshot connected to, never the password.
    fn remembered_source(&self) -> RememberedImportSource {
        RememberedImportSource {
            provider: source_kind_label(self.source_kind).to_string(),
            host: self.host.clone(),
            port: self.port,
            tls_mode: tls_mode_label(self.tls_mode).to_string(),
            username: self.username.clone(),
        }
    }

    /// Prefill the Source step from what the account remembers — a preset
    /// provider keeps its own server, as `SelectSourceKind` would set it.
    fn prefill_from(&mut self, remembered: &RememberedImportSource) {
        let Some((kind, tls, r)) = parse_remembered(remembered) else {
            return;
        };
        self.source_kind = kind;
        self.username = r.username.clone();
        match provider_preset(kind) {
            Some((host, port)) => {
                self.host = host.to_string();
                self.port = port;
                self.tls_mode = ImportTlsMode::Implicit;
            }
            None => {
                self.host = r.host.clone();
                self.port = r.port;
                self.tls_mode = tls;
            }
        }
    }

    /// The Source step's fields back to a first visit's: the Gmail preset and
    /// no username — what the user's Forget leaves on screen.
    fn clear_source_fields(&mut self) {
        let empty = Self::empty();
        self.source_kind = empty.source_kind;
        self.host = empty.host;
        self.port = empty.port;
        self.tls_mode = empty.tls_mode;
        self.username = empty.username;
        self.password = SecretString::default();
    }
}

impl MailImportSnapshot {
    /// A fresh app adopting a live session (§ Resume protocol steps 2–3):
    /// the source it reconnects to from the row's descriptor — a provider's
    /// own server and port, or Generic's recorded host with the port and
    /// connection security the account remembers for that host and username
    /// (§ Credential handling; the row records neither), on the 993
    /// implicit-TLS default only when nothing matching is remembered — and
    /// the walk from the row's scope, every mailbox selected (the scope IS
    /// the selection). The since date arrives through [`Self::apply_session`].
    /// Nothing about the credential: Resume asks.
    fn adopt_session_source(
        &mut self,
        s: &ImportSessionView,
        remembered: Option<&RememberedImportSource>,
    ) {
        if let Some((kind, username, host)) = parse_source_descriptor(&s.source_descriptor) {
            self.tls_mode = ImportTlsMode::Implicit;
            match provider_preset(kind) {
                Some((preset_host, port)) => {
                    self.host = preset_host.to_string();
                    self.port = port;
                }
                None => {
                    let matching = remembered
                        .and_then(parse_remembered)
                        .filter(|(k, _, r)| *k == kind && r.host == host && r.username == username);
                    match matching {
                        Some((_, tls, r)) => {
                            self.port = r.port;
                            self.tls_mode = tls;
                        }
                        None => self.port = 993,
                    }
                    self.host = host;
                }
            }
            self.source_kind = kind;
            self.username = username;
        }
        self.mailboxes = s
            .scope
            .iter()
            .map(|name| SourceMailboxOption {
                name: name.clone(),
                selected: true,
                message_count: 0,
                byte_total: None,
            })
            .collect();
        self.password = SecretString::default();
        self.resume_needs_credentials = true;
    }
}

/// Actions the per-app UI dispatches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum MailImportAction {
    /// Page load / resume: jump to the newest live (`running` / `paused`)
    /// session's Progress screen; with none, to the newest failed one's, as a
    /// record. A `completed` session is not adopted.
    Refresh,
    /// Step 1 — pick the provider; pre-fills host/port/tls_mode for a preset.
    SelectSourceKind { kind: ImportSourceKind },
    /// Step 1 (Generic) — hostname.
    SetHost { value: String },
    /// Step 1 (Generic) — port.
    SetPort { value: u16 },
    /// Step 1 (Generic) — TLS mode.
    SetTlsMode { mode: ImportTlsMode },
    /// Step 1 — username (app-password account or generic IMAP user).
    SetUsername { value: String },
    /// Step 1 — password / app-password.
    SetPassword { value: SecretString },
    /// Step 1 — forget the remembered source
    /// (`mail-import-source-forget-button`, § Credential handling): the
    /// account's record is deleted and the Source step's fields return to a
    /// first visit's. Refused when the machine has no store to forget from.
    ForgetSource,
    /// Step 1 (Outlook) — ask Microsoft for a sign-in code
    /// (`mail-import-source-oauth-button`) and show it on
    /// [`MailImportSnapshot::microsoft_signin`]; the app glue then spawns
    /// [`MailImportMachine::run_microsoft_sign_in`], whose grant connects by
    /// itself. Refused with a sentence while the sign-in is unavailable or no
    /// address is typed.
    BeginMicrosoftSignIn,
    /// Step 1 (Outlook) — stop a sign-in under way and forget any token.
    CancelMicrosoftSignIn,
    /// Step 1→3 — `LOGIN` + `LIST` against the source
    /// (`mail-import-connect-button`). On success advances to Scope; on
    /// failure stays on Source with `error` set.
    Connect,
    /// Step 3 — toggle one mailbox's selection.
    ToggleMailbox { mailbox: String },
    /// Step 3 — set the "since" date (empty clears).
    SetDateFrom { value: String },
    /// Step 3 — max message size in bytes.
    SetMaxSizeBytes { value: u64 },
    /// Step 3 — the advanced "Import duplicates anyway" option
    /// (`mail-import-scope-import-duplicates-toggle`, § Opt-out per session).
    /// Takes effect at `Start`, which records it on the session row.
    SetImportDuplicates { value: bool },
    /// Advance Scope → Confirm (client-side; no round-trip).
    Next,
    /// Go back Confirm → Scope → Source (client-side).
    Back,
    /// Step 4 — durable commit: open the `import_sessions` row
    /// (`start_import_session`) and move to the Progress screen.
    Start,
    /// Step 5 — pause the running session.
    Pause,
    /// Step 5 — resume the paused session.
    Resume,
    /// Step 5 — cancel (confirms client-side first; already-imported
    /// messages are kept, § UX shape step 5).
    Cancel,
    /// Step 5 — restart every mailbox in
    /// [`MailImportSnapshot::rebuilt_mailboxes`]
    /// (`mail-import-restart-button`, § Resume protocol step 4): forget its
    /// cursor and resume the paused session, so the run walks it again from
    /// its first message — the nest's dedup skips what already arrived
    /// (§ Dedup vs. existing mail). The app spawns `run_import` after it
    /// exactly as after a `Resume`. Refused while no mailbox is pending.
    RestartRebuiltMailboxes,
    /// Step 5, the full-mailbox notice (`mail-import-quota-skip-rest-button`,
    /// § Failure handling → *Nest-side errors*): take a paused import up
    /// again, recording every message it has not taken yet as skipped — a
    /// plain skip, never `quota_exceeded`, so the nest's quota hold lifts —
    /// and finish the session. Like `Resume`, the app then spawns
    /// [`MailImportMachine::run_import`], which walks the rest by UID alone.
    SkipRest,
    /// Step 5 — a FAILED import's one control (`mail-import-dismiss-button`,
    /// § A failed import is a record the wizard shows until the user moves
    /// on): delete the record (`discard_import_session`) and return to Source.
    /// Refused with a sentence on a live import, which is cancelled instead.
    Dismiss,
}

/// Step 1→2: the whole Source form as one ordered multi-action dispatch —
/// `SetHost`/`SetPort` only for the kinds that show them, then always
/// `SetUsername`, `SetPassword`, `Connect`. Every app hand-copied this exact
/// sequence (tui/linux natively, web in TypeScript, each one citing the
/// others as its mirror) because each reads its own already-resolved field
/// values differently — a plain state struct on tui, live `Entry::text()`
/// reads (routed through whichever of the password/app-password widgets the
/// picked `kind` shows) on linux — so this is the one place the wire-action
/// shape itself lives; callers stay responsible for locating their own
/// strings.
///
/// **Precondition for the three presets (`Gmail`/`Outlook`/`ICloud`): the
/// caller must already have dispatched `SelectSourceKind { kind }` to the
/// same machine for the picked kind.** Omitting `SetHost`/`SetPort` here is
/// safe only because `apply_client_action`'s `SelectSourceKind` arm is what
/// wrote `host`/`port`/`tls_mode` via `provider_preset` — this function does
/// not derive the connect-time host from `kind` itself, it trusts the
/// snapshot already carries it. A caller that keeps the picked kind only in
/// local UI state and never dispatches `SelectSourceKind` sends a
/// stale/attacker host with a freshly-typed credential (mailbox-migration.md
/// § Wizard steps). Outlook joined the presets with the 2026-10-09 ruling
/// (§ Microsoft sign-in — the device-code grant): with the password fallback
/// withdrawn it has no host field to type into, and it signs in with
/// Microsoft rather than a password, so its `Connect` needs a granted token
/// ([`MailImportAction::BeginMicrosoftSignIn`]). Only `Generic` sends the
/// caller's `host`/`port`.
///
/// Exported over UniFFI so the two bindings-consuming apps reach the same
/// sequence the native ones do: without it windows (C#) and android (Kotlin)
/// can only hand-roll a seventh and eighth copy of the ordering, which is the
/// exact per-app divergence this lift removed (priority #2/#4).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn connect_actions(
    kind: ImportSourceKind,
    host: &str,
    port: &str,
    username: &str,
    password: SecretString,
) -> Vec<MailImportAction> {
    let mut actions = Vec::new();
    if kind == ImportSourceKind::Generic {
        actions.push(MailImportAction::SetHost {
            value: host.trim().to_string(),
        });
        if let Ok(port) = port.trim().parse::<u16>() {
            actions.push(MailImportAction::SetPort { value: port });
        }
    }
    actions.push(MailImportAction::SetUsername {
        value: username.trim().to_string(),
    });
    actions.push(MailImportAction::SetPassword { value: password });
    actions.push(MailImportAction::Connect);
    actions
}

/// Step 1's **Sign in with Microsoft** (`mail-import-source-oauth-button`) as
/// one ordered dispatch — [`connect_actions`]' twin for the Outlook source:
/// commit the account's address the field holds (the XOAUTH2 user), then ask
/// Microsoft for a code. The app glue spawns
/// [`MailImportMachine::run_microsoft_sign_in`] once the dispatch leaves the
/// sign-in [`MicrosoftSignInState::Waiting`], and that grant connects by
/// itself (`mailbox-migration.md` § Microsoft sign-in — the device-code
/// grant). On the Progress step of an adopted session the address is the
/// session's own, so the caller dispatches a bare `BeginMicrosoftSignIn`.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn microsoft_sign_in_actions(username: &str) -> Vec<MailImportAction> {
    vec![
        MailImportAction::SetUsername {
            value: username.trim().to_string(),
        },
        MailImportAction::BeginMicrosoftSignIn,
    ]
}

/// Step 5's Resume as one ordered dispatch — [`connect_actions`]' twin for
/// the Progress step, over the snapshot's `resume_needs_credentials` and
/// `source_kind`. While [`MailImportSnapshot::resume_needs_credentials`] is
/// set (an adopted session, § Resume protocol step 2) the credential the step
/// painted is committed first: Generic's port draft when it parses, then the
/// password (an Outlook token lives in the machine, so its password is
/// ignored there). Otherwise a bare `Resume`. Callers locate their own
/// strings, as for `connect_actions`; the connection-security picker
/// dispatches `SetTlsMode` itself, as on the Source step.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn resume_actions(
    needs_credentials: bool,
    kind: ImportSourceKind,
    port: &str,
    password: SecretString,
) -> Vec<MailImportAction> {
    let mut actions = Vec::new();
    if needs_credentials {
        if kind == ImportSourceKind::Generic
            && let Ok(port) = port.trim().parse::<u16>()
        {
            actions.push(MailImportAction::SetPort { value: port });
        }
        actions.push(MailImportAction::SetPassword { value: password });
    }
    actions.push(MailImportAction::Resume);
    actions
}

/// Whether a successful dispatch of `action` obliges the app to spawn
/// [`MailImportMachine::run_import`] once: `Start`, `Resume`,
/// `RestartRebuiltMailboxes` (a Resume with the rebuilt mailboxes' cursors
/// dropped) and `SkipRest` (a Resume that walks the rest recording skips).
/// The one list every app asks, so a new run-starting action reaches all
/// seven at once.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn import_action_starts_run(action: MailImportAction) -> bool {
    matches!(
        action,
        MailImportAction::Start
            | MailImportAction::Resume
            | MailImportAction::RestartRebuiltMailboxes
            | MailImportAction::SkipRest
    )
}

/// The max-size field's pre-fill text, in **MB** — the string form of the
/// same 50 MiB default [`MailImportSnapshot::default`] and
/// [`scope_next_actions`]'s own parse fallback carry in bytes. Both apps
/// hand-copied this exact literal for their input field's initial value;
/// collapsed here, the `SOURCE_KINDS`/`TLS_MODES` precedent.
pub const DEFAULT_MAX_SIZE_MB: &str = "50";

/// Step 2/3→3/4: commit both Scope-step drafts, then advance. An unparseable
/// max-size buffer falls back to the machine's own default rather than
/// sending a stale/zero value. Both apps hand-copied this exact parse-then-
/// dispatch shape (round 87 lift) because each reads its own raw input
/// differently — a plain state field on tui, a live `Entry::text()` read on
/// linux — so callers stay responsible for locating their own strings, same
/// division of labor as [`connect_actions`]. Exported over UniFFI for the same
/// reason that one is.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn scope_next_actions(date_from: &str, max_size_input: &str) -> Vec<MailImportAction> {
    let max_size_bytes = max_size_input
        .trim()
        .parse::<u64>()
        .ok()
        .and_then(|mb| mb.checked_mul(1024 * 1024))
        .unwrap_or(50 * 1024 * 1024);
    vec![
        MailImportAction::SetDateFrom {
            value: date_from.trim().to_string(),
        },
        MailImportAction::SetMaxSizeBytes {
            value: max_size_bytes,
        },
        MailImportAction::Next,
    ]
}

/// One message's outcome from [`MailImportNest::send_unit`] — a minimal
/// per-message projection of
/// `fauna_protocol::bridge_routing::ImportMessageOutcome`, dropping its
/// `Imported` variant's `message_id`/`uid`/`uid_validity` (the error budget
/// and the progress counters never need them; the real `rpc_glue` impl maps
/// straight from the wire enum).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    Imported,
    /// § Progress lives nest-side: dedup-skip, oversize-skip, user-scope-skip.
    Skipped {
        reason: String,
    },
    /// § Failure handling's nest-side table: parse fail, quota, vanished
    /// mailbox, … — never session-fatal by itself (only the § Per-message
    /// error budget can stop the run over these).
    Errored {
        reason: String,
    },
}

/// [`MailImportNest::send_unit`]'s reply — index-aligned with the sent
/// [`ImportUnit`] (one entry for a `Single`, one per message for a `Batch`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendUnitReply {
    pub outcomes: Vec<SendOutcome>,
}

/// One [`ImportSourceNest::fetch_window`] answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchWindow {
    /// Every message of the window under the size limit — fetched, or failed
    /// at the source — in UID order.
    pub outcomes: Vec<FetchOutcome>,
    /// Every message of the window over the user's size limit, by its
    /// `RFC822.SIZE`: left unfetched (§ Per-message flow step 1), its body
    /// never transferred.
    pub too_large: Vec<UidEntry>,
}

impl FetchWindow {
    /// No message left after the cursor: the mailbox is done.
    pub fn is_empty(&self) -> bool {
        self.outcomes.is_empty() && self.too_large.is_empty()
    }
}

/// WS-RPC seam to nest's `import_sessions` surface. **Real, not stubbed** —
/// the nest-side backend has shipped since 2026-07-08 (§ Implementation
/// status today). The `rpc_glue` impl wraps
/// `fauna_mail::imap_client::MailImportClient` one-to-one. Dual `async_trait`
/// arm + `MaybeSendSync` supertrait so the one seam serves native + wasm
/// (mirrors `MailExportNest`).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait MailImportNest: MaybeSendSync {
    /// `fauna.bridges.list_import_sessions` — resume protocol step 1.
    async fn list_sessions(&self) -> Result<Vec<ImportSessionView>, NestError>;
    /// The deployment's own Microsoft app registration, `None` when the admin
    /// set none (`mailbox-migration.md` § Microsoft sign-in, *An
    /// admin-supplied registration*). Answered from the `fauna.setup.status`
    /// reply (`SetupStatusReply::microsoft_registration`) — no kind of its
    /// own. Read at every [`MailImportMachine`] refresh, beside
    /// [`Self::list_sessions`].
    async fn microsoft_registration(&self) -> Result<Option<MicrosoftRegistration>, NestError>;
    /// `fauna.bridges.start_import_session`. `scope` is every mailbox the
    /// wizard's scope step selected (§ Resume protocol); `date_from` is the
    /// scope step's since date, empty for unbounded (§ Wizard steps step 3);
    /// `skip_dedup` is the duplicates choice the row records
    /// (§ Opt-out per session).
    async fn start_session(
        &self,
        source_descriptor: String,
        total_count: u64,
        scope: Vec<String>,
        date_from: String,
        skip_dedup: bool,
    ) -> Result<ImportSessionView, NestError>;
    /// `fauna.bridges.pause_import_session`.
    async fn pause_session(&self, session_id: String) -> Result<ImportSessionView, NestError>;
    /// `fauna.bridges.resume_import_session`.
    async fn resume_session(&self, session_id: String) -> Result<ImportSessionView, NestError>;
    /// `fauna.bridges.cancel_import_session` — already-imported messages are
    /// kept.
    async fn cancel_session(&self, session_id: String) -> Result<ImportSessionView, NestError>;
    /// `fauna.bridges.discard_import_session` — a failed import's Dismiss
    /// (§ A failed import is a record the wizard shows until the user moves
    /// on). Deletes a terminal row; `Ok(false)` is a row already gone.
    async fn discard_session(&self, session_id: String) -> Result<bool, NestError>;
    /// `fauna.bridges.finalize_import_session`. Not yet dispatched by this
    /// module (no `MailImportAction` variant calls it) — the future
    /// `run_import` calls it at end-of-enumeration.
    async fn finalize_session(&self, session_id: String) -> Result<ImportSessionView, NestError>;
    /// `fauna.bridges.fail_import_session`. Called by
    /// [`MailImportMachine::run_import`] on a § Per-message error budget
    /// breach or a since date the session can no longer read — never on a
    /// lost connection, which pauses (§ Failure handling).
    async fn fail_session(
        &self,
        session_id: String,
        reason: String,
    ) -> Result<ImportSessionView, NestError>;

    /// `fauna.bridges.import_message` / `_message_batch`, dispatched by
    /// `unit`'s variant (§ Batching). `skip_dedup` is the session's
    /// duplicates choice ([`MailImportSnapshot::import_duplicates`],
    /// § Opt-out per session). The real
    /// `rpc_glue` impl wraps `fauna_mail::imap_client::MailImportClient::send_unit`,
    /// projecting its wire reply's `ImportMessageOutcome`(s) down to
    /// [`SendOutcome`].
    /// `revised_total_count` replaces the session's stored estimate; `None`
    /// leaves it alone. `run_import` sends `None` today: the Confirm
    /// estimate the session opens with already counts only what the range
    /// admits (§ Progress lives nest-side's `total_count`).
    async fn send_unit(
        &self,
        session_id: String,
        unit: ImportUnit,
        skip_dedup: bool,
        revised_total_count: Option<u64>,
    ) -> Result<SendUnitReply, NestError>;

    /// `fauna.bridges.record_import_outcomes` — the messages of `mailbox` the
    /// loop decided about without sending (over the size limit, unreadable at
    /// the source), so the nest's counters and cursor carry them: the session
    /// row, not this client, is what a restart and every other device read
    /// (§ Progress lives nest-side). Answers the session's counters after the
    /// call.
    async fn record_client_outcomes(
        &self,
        session_id: String,
        mailbox: String,
        uid_validity: u32,
        outcomes: Vec<ClientImportOutcome>,
    ) -> Result<RecordImportOutcomesReply, NestError>;
}

/// What went wrong when the source would not connect, as the person reads it
/// (`mailbox-migration.md` § Wizard steps step 2): one plain sentence per
/// class, never the server's own words.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SourceConnectFailure {
    /// The server answered and refused the sign-in — a wrong password, a
    /// provider that wants an app password, IMAP switched off.
    SignIn,
    /// Nothing answered: a wrong hostname or port, or no network.
    Reach,
    /// It answered, but the connection could not be secured — the TLS
    /// handshake, the certificate, or a refused `STARTTLS`.
    Secure,
}

/// [`ImportSourceNest::connect`]'s failure: the class the wizard shows, and
/// the source's own words, which go to the app's log only.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{failure:?}: {detail}")]
pub struct SourceConnectError {
    pub failure: SourceConnectFailure,
    pub detail: String,
}

impl SourceConnectError {
    pub fn new(failure: SourceConnectFailure, detail: impl Into<String>) -> Self {
        Self {
            failure,
            detail: detail.into(),
        }
    }
}

/// How the source is signed in to: `LOGIN` with a password, or — the Outlook
/// source (§ Microsoft sign-in — the device-code grant) — `AUTHENTICATE
/// XOAUTH2` with a Microsoft access token. Both are § Credential handling's
/// client-memory-only credentials, zeroized on drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceAuth {
    /// `LOGIN` as [`SourceConnectParams::username`].
    Password(SecretString),
    /// `AUTHENTICATE XOAUTH2`; `user` is the address the person typed — the
    /// app never decodes the token to learn it.
    MicrosoftToken {
        user: String,
        access_token: SecretString,
    },
}

/// The parameters `mail-import-source-*` collects, handed to
/// [`ImportSourceNest::connect`]. Never carries anything beyond what §
/// Credential handling says lives in client memory.
#[derive(Debug, Clone)]
pub struct SourceConnectParams {
    pub kind: ImportSourceKind,
    pub host: String,
    pub port: u16,
    pub tls_mode: ImportTlsMode,
    pub username: String,
    pub auth: SourceAuth,
}

/// § Failure handling's TCP/TLS retry backoff (5 s / 30 s / 2 min) as an
/// injectable seam — `run_import`'s tests prove the *requested* delays
/// (`testing.md` convention 14: never a real wall-clock wait), and the real
/// impl is `fauna_sleep::sleep`. Mirrors
/// `fauna_mail::imap_client::ImapClock`'s shape, but as its own dyn-safe
/// `async_trait` (unlike `ImapClock`'s AFIT) since [`MailImportMachine`]
/// holds every seam as `Arc<dyn _>`; the Microsoft sign-in's poll cadence,
/// its code's lifetime and the token's expiry ride the same clock.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait RetryClock: MaybeSendSync {
    /// Sleep for at least `ms` milliseconds.
    async fn sleep_ms(&self, ms: u64);
    /// Now, in epoch milliseconds — the frame
    /// [`MicrosoftSignInView::expires_at_ms`] is painted in.
    fn now_ms(&self) -> u64;
}

/// The Microsoft sign-in's HTTPS seam, dyn-safe for the machine — the
/// platform shell's [`fauna_mail::imap_client::OAuthHttp`] behind
/// [`MailImportMachine::with_oauth_http`] (native: `NativeOAuthHttp` over
/// `reqwest`). A machine built without one — web, until the relay lands
/// (§ Where the IMAP client runs) — answers that the sign-in is not
/// available.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait ImportOAuthHttp: MaybeSendSync {
    /// One form POST; see [`fauna_mail::imap_client::OAuthHttp::post_form`].
    /// The error is rendered for the log.
    async fn post_form(&self, url: &str, form: &str) -> Result<OAuthHttpResponse, String>;
}

/// The typed not-yet: a platform with no HTTPS seam for the sign-in.
struct NoOAuthHttp;

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl ImportOAuthHttp for NoOAuthHttp {
    async fn post_form(&self, _url: &str, _form: &str) -> Result<OAuthHttpResponse, String> {
        Err("this platform has no Microsoft sign-in transport yet".into())
    }
}

/// Adapts the machine's dyn seam to the shared flow's AFIT one.
struct DynOAuthHttp<'a>(&'a dyn ImportOAuthHttp);

impl fauna_mail::imap_client::OAuthHttp for DynOAuthHttp<'_> {
    type Error = String;

    async fn post_form(&self, url: &str, form: &str) -> Result<OAuthHttpResponse, String> {
        self.0.post_form(url, form).await
    }
}

/// Adapts the machine's [`RetryClock`] to the shared flow's `ImapClock`.
struct DynClock<'a>(&'a dyn RetryClock);

impl ImapClock for DynClock<'_> {
    fn now_ms(&self) -> u64 {
        self.0.now_ms()
    }

    async fn sleep_ms(&self, ms: u64) {
        self.0.sleep_ms(ms).await;
    }
}

/// § Microsoft sign-in, *Lifetime and refresh*: a reconnect refreshes the
/// access token when it has under five minutes left.
const TOKEN_REFRESH_MARGIN_MS: u64 = 5 * 60 * 1_000;

/// The granted tokens, in the machine's private state only — never in the
/// snapshot, never in a request to the nest. Dropping it zeroizes both.
struct HeldToken {
    user: String,
    access_token: SecretString,
    refresh_token: Option<SecretString>,
    expires_at_ms: u64,
}

impl HeldToken {
    fn auth(&self) -> SourceAuth {
        SourceAuth::MicrosoftToken {
            user: self.user.clone(),
            access_token: self.access_token.clone(),
        }
    }
}

/// The sign-in's private half.
#[derive(Default)]
struct SignInState {
    /// A started flow waiting for `run_microsoft_sign_in` to poll it.
    flow: Option<DeviceFlow>,
    /// Set by `CancelMicrosoftSignIn`; the polling loop reads it.
    cancelled: bool,
    token: Option<HeldToken>,
    /// A Resume happened: the next run's first source call may reconnect.
    reconnect_pending: bool,
    /// A refresh was refused mid-run; the loop ends the session.
    sign_in_again: bool,
    /// The registration's endpoints the flow was started with. The token's
    /// refresh and the consent sentence use these, so an admin changing the
    /// registration mid-import never sends one client's refresh token under
    /// another client's ID.
    client: Option<OAuthClient>,
}

/// What a reconnect's refresh came to.
enum RefreshOutcome {
    /// No token, or plenty of time left — or a fresh one now in hand.
    Fresh,
    /// Microsoft could not be reached; the reconnect goes ahead and the
    /// retry ladder tries again.
    Unreachable,
    /// Microsoft refused the refresh: the person must sign in again.
    Refused,
}

/// The foreign-IMAP-server seam (`mailbox-migration.md` § Client-driven
/// streaming model). The real `rpc_glue` impl wraps a live
/// `fauna_mail::imap_client::ImapSession`, generic over the platform's own
/// `ImapTransport`/`ImapClock` — this crate never names either directly (§
/// Where the IMAP client runs: native TCP+rustls vs web sans-io-over-relay).
///
/// What the wizard shell (Source → Scope) needs, plus what
/// [`MailImportMachine::run_import`]'s fetch-drive loop needs once a session
/// is `Running`.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait ImportSourceNest: MaybeSendSync {
    /// `LOGIN` with a [`SourceAuth::Password`], or `AUTHENTICATE XOAUTH2`
    /// with a [`SourceAuth::MicrosoftToken`], against the source. Refuses a
    /// plaintext-downgrade exactly as `ImapSession` does.
    /// The seam may keep `params` until [`Self::logout`] (§ Credential
    /// handling: the source password lives in client memory until LOGOUT),
    /// so that a connection a fault dropped can be opened again; the wizard's
    /// own copy is cleared once this succeeds. A failure is classed for the
    /// wizard's sentence (§ Wizard steps step 2).
    async fn connect(&self, params: SourceConnectParams) -> Result<(), SourceConnectError>;
    /// `LIST "" "*"` — every mailbox the account can see, `\Noselect`
    /// containers included (this module filters them, § `SourceMailboxOption`
    /// docs). `LIST` carries no counts: the Confirm estimate is
    /// [`Self::estimate_mailbox`]'s.
    async fn list_source_mailboxes(&self) -> Result<Vec<SourceMailboxView>, NestError>;

    /// How many messages of `mailbox` an import with since date `since_epoch`
    /// (inclusive, epoch seconds; `None` = unbounded) would take, and their
    /// bytes — `ImapSession::estimate_mailbox`'s answer, asked once per
    /// selected mailbox at Scope → Confirm (`mailbox-migration.md` § Wizard
    /// steps step 4). [`NestError::Transient`] for a connection fault,
    /// [`NestError::Rejected`] when the source refuses the question for this
    /// mailbox.
    async fn estimate_mailbox(
        &self,
        mailbox: &str,
        since_epoch: Option<i64>,
    ) -> Result<MailboxEstimate, NestError>;

    /// `EXAMINE` `mailbox` read-only (never `SELECT` — § per-message flow
    /// step 1's precedent: the source must be left exactly as the user found
    /// it), reporting its UIDVALIDITY as the source holds it now. The seam
    /// does not judge it: comparing it against the cursor is the loop's
    /// (§ Resume protocol step 4 — a mismatch is a rebuilt mailbox, which the
    /// person is offered to restart). [`NestError::Rejected`] when the source
    /// refuses the mailbox (it vanished, …) — per-mailbox, not session-fatal:
    /// the caller logs it and moves to the next mailbox.
    async fn examine(&self, mailbox: &str) -> Result<MailboxStatus, NestError>;

    /// One throttled fetch window (§ Throttling — the real seam owns the
    /// per-source-server rate cap internally): at most `max` messages
    /// starting after `cursor`'s `last_processed_source_uid` (from the
    /// mailbox's first message when `cursor` is `None`). A message whose
    /// reported size is over `max_size_bytes` is listed in
    /// [`FetchWindow::too_large`] and its body is never fetched (§ Per-message
    /// flow step 1); one whose size the source does not report is fetched. An
    /// empty window means the mailbox is exhausted. [`NestError::Transient`] for a
    /// retryable TCP/TLS fault (§ Failure handling's 3-retry backoff);
    /// [`NestError::Rejected`] for a session-fatal source error (e.g. the
    /// source auth expired mid-run).
    async fn fetch_window(
        &self,
        mailbox: &str,
        cursor: Option<MailboxCursor>,
        max: usize,
        max_size_bytes: u64,
    ) -> Result<FetchWindow, NestError>;

    /// The enumeration alone: at most `max` of the mailbox's messages after
    /// `cursor`, each with the size and date the source reports, and no body
    /// fetched — what "Skip the rest" walks ([`MailImportAction::SkipRest`]).
    /// Same calling contract and errors as [`Self::fetch_window`]; an empty
    /// answer means the mailbox is exhausted.
    async fn enumerate_window(
        &self,
        mailbox: &str,
        cursor: Option<MailboxCursor>,
        max: usize,
    ) -> Result<Vec<UidEntry>, NestError>;

    /// `LOGOUT` — § Credential handling: ends the source session and the
    /// lifetime of the source credentials this run held. Called once
    /// `run_import` stops for any reason (done, paused, cancelled, errored).
    async fn logout(&self) -> Result<(), NestError>;

    /// Whether this seam's transport could ever succeed, independent of the
    /// credentials given. `false` only for a platform whose transport is not
    /// built yet (currently: web — § Where the IMAP client runs). §
    /// Credential handling's retain-on-failure policy exists so a user can
    /// retry without retyping; a seam that answers `false` here can never
    /// reach that retry, so [`MailImportMachine::connect`] clears the
    /// password on failure instead of retaining it. Every seam with a real
    /// transport keeps the default.
    fn can_ever_connect(&self) -> bool {
        true
    }

    /// Hand the seam a refreshed credential for the connection it keeps
    /// (§ Microsoft sign-in, *Lifetime and refresh*): the next time it opens
    /// the source again it signs in with this. A live connection is left
    /// alone — an authenticated IMAP connection outlives its token. The
    /// default suits a seam that keeps nothing.
    async fn replace_auth(&self, _auth: SourceAuth) {}

    /// Whether the source refused the kept credential when this seam signed
    /// in again part-way through a run (§ Failure handling → *Source-side
    /// errors*, the `LOGIN` row); taken once. The seam answers that call
    /// [`NestError::Rejected`] and sets this, so the loop ends the session
    /// asking for new credentials rather than skipping the mailbox or
    /// retrying a password the source has stopped taking. The default suits
    /// a seam that never signs in again.
    fn take_sign_in_refused(&self) -> bool {
        false
    }
}

/// § Failure handling: up to 3 retries with exponential backoff (5 s, 30 s,
/// 2 min) before the loop pauses the session — the source's TCP/TLS faults
/// and a dropped connection to the nest alike.
const RETRY_DELAYS_MS: [u64; 3] = [5_000, 30_000, 120_000];

/// One [`ImportSourceNest::fetch_window`] call's message cap. Chosen equal to
/// [`fauna_mail::imap_client::MAX_BATCH_MESSAGES`] so a window's fetched
/// messages pack into at most two [`ImportUnit`]s (the common case: one) —
/// keeps "drain the in-flight window on Pause" a small, bounded amount of
/// work.
const FETCH_WINDOW_SIZE: usize = fauna_mail::imap_client::MAX_BATCH_MESSAGES;

/// § Per-message error budget: "More than 10% of messages errored in the
/// most recent 1000-message window."
const ERROR_BUDGET_WINDOW: usize = 1000;
const ERROR_BUDGET_FRACTION: f64 = 0.10;
/// § Per-message error budget: "More than 50 consecutive messages errored."
const ERROR_BUDGET_CONSECUTIVE: u32 = 50;
/// A floor on the fraction check the doc does not spell out but production
/// correctness needs: without one, a single early failure in a small import
/// (1 of 1 processed so far = 100%) trips the budget on its own, which is
/// never what "10% of the most recent 1000" means for an import whose total
/// size the client cannot know in advance. Tied to the consecutive
/// threshold's own scale rather than inventing an unrelated number — below
/// this many samples, only the consecutive-run check can fire.
const ERROR_BUDGET_MIN_SAMPLE: usize = ERROR_BUDGET_CONSECUTIVE as usize;

/// A rolling window of per-message pass/fail outcomes (§ Per-message error
/// budget) — both breach conditions in one place so `run_import` has a single
/// call site per outcome.
#[derive(Debug, Default)]
struct ErrorBudget {
    /// `true` = errored. Capped at [`ERROR_BUDGET_WINDOW`].
    window: VecDeque<bool>,
    consecutive_errors: u32,
}

impl ErrorBudget {
    /// Record one message's outcome. Returns `true` when either threshold is
    /// now breached.
    fn record(&mut self, errored: bool) -> bool {
        self.consecutive_errors = if errored {
            self.consecutive_errors + 1
        } else {
            0
        };
        self.window.push_back(errored);
        if self.window.len() > ERROR_BUDGET_WINDOW {
            self.window.pop_front();
        }
        let errors = self.window.iter().filter(|&&e| e).count();
        let fraction_breach = self.window.len() >= ERROR_BUDGET_MIN_SAMPLE
            && f64::from(u32::try_from(errors).unwrap_or(u32::MAX))
                > ERROR_BUDGET_FRACTION * f64::from(u32::try_from(self.window.len()).unwrap_or(1));
        fraction_breach || self.consecutive_errors > ERROR_BUDGET_CONSECUTIVE
    }
}

/// What [`MailImportMachine::send_and_record`] learned from one unit's reply.
#[derive(Debug, Default)]
struct SentUnit {
    /// The § Per-message error budget is now breached.
    breached: bool,
    /// The first message the nest answered `quota_exceeded`, if any.
    quota_exceeded_at: Option<u32>,
}

/// A packed unit's source uids, index-aligned with its reply's outcomes.
fn unit_uids(unit: &ImportUnit) -> Vec<u32> {
    unit_items(unit).map(|i| i.source_uid).collect()
}

/// A packed unit's messages, in the order its reply's outcomes come back.
fn unit_items(unit: &ImportUnit) -> impl Iterator<Item = &ImportMessageItem> {
    match unit {
        ImportUnit::Batch(items) => items.as_slice(),
        ImportUnit::Single(item) => std::slice::from_ref(item.as_ref()),
    }
    .iter()
}

/// One skip-log line (§ Wizard steps step 5's per-message error log): which
/// message, and why — the shape every line the run records takes.
fn skip_log_line(mailbox: &str, uid: u32, reason: &str) -> String {
    format!("{mailbox} uid {uid}: {reason}")
}

/// What `error-message` says when the source would not connect (§ Wizard
/// steps step 2): the failure's sentence, then — for the three providers whose
/// sign-in rules are known — that provider's hint.
fn connect_failure_sentence(kind: ImportSourceKind, failure: SourceConnectFailure) -> String {
    use fauna_i18n::strings::mail_import as s;
    let sentence = match failure {
        SourceConnectFailure::SignIn => s::CONNECT_FAILED_SIGN_IN,
        SourceConnectFailure::Reach => s::CONNECT_FAILED_REACH,
        SourceConnectFailure::Secure => s::CONNECT_FAILED_SECURE,
    };
    let hint = match kind {
        ImportSourceKind::Gmail => Some(s::CONNECT_HINT_GMAIL),
        ImportSourceKind::Outlook => Some(s::CONNECT_HINT_OUTLOOK),
        ImportSourceKind::ICloud => Some(s::CONNECT_HINT_ICLOUD),
        ImportSourceKind::Generic => None,
    };
    match hint {
        Some(hint) => format!("{sentence} {hint}"),
        None => sentence.to_string(),
    }
}

/// The session's end when Microsoft refused a token refresh at a reconnect
/// (§ Failure handling).
fn sign_in_again() -> String {
    fauna_i18n::strings::mail_import::MICROSOFT_SESSION_SIGN_IN_AGAIN.to_string()
}

/// The session's end when the per-message error budget is spent (§
/// Per-message error budget); the per-message reasons stay on the log.
fn error_budget_exceeded() -> String {
    fauna_i18n::strings::mail_import::ERROR_BUDGET_EXCEEDED.to_string()
}

/// Where an organisation's administrator approves the project's
/// registration, shown with the consent refusal's sentence.
fn admin_consent_url(client: &OAuthClient) -> String {
    oauth_device::admin_consent_url(&client.client_id)
}

/// The sentence `error-message` shows for a pause the loop made itself.
fn pause_reason_sentence(reason: ImportPauseReason) -> &'static str {
    match reason {
        ImportPauseReason::MailboxFull => fauna_i18n::strings::mail_import::PAUSED_MAILBOX_FULL,
        ImportPauseReason::ConnectionLost => {
            fauna_i18n::strings::mail_import::PAUSED_CONNECTION_LOST
        }
        ImportPauseReason::MailboxRebuilt => {
            fauna_i18n::strings::mail_import::PAUSED_MAILBOX_REBUILT
        }
    }
}

/// Whether [`MailImportMachine::run_import`] should stop at its next
/// checkpoint, and how — set by `pause()`/`cancel()`, read by the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum StopRequest {
    #[default]
    None,
    /// Drain the in-flight fetch window to nest, then stop.
    Pause,
    /// Abort without draining the in-flight window.
    Cancel,
}

/// One instance per user client. Holds the rendered snapshot; drives both
/// seams.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MailImportMachine {
    nest: Arc<dyn MailImportNest>,
    source: Arc<dyn ImportSourceNest>,
    clock: Arc<dyn RetryClock>,
    inner: Mutex<MailImportSnapshot>,
    /// The active session's id. Kept out of the public snapshot for
    /// symmetry with `MailExportMachine` (the machine needs it to drive
    /// pause/resume/cancel; the UI never keys off it directly).
    session_id: Mutex<Option<String>>,
    /// `run_import`'s per-mailbox resume position (module docs, *Resume*).
    /// Reset on a fresh `Start`, seeded from the row on a cold adoption;
    /// survives `Pause`/`Resume`.
    cursors: Mutex<HashMap<String, MailboxCursor>>,
    /// § Per-message error budget's rolling window. Same lifetime as
    /// `cursors` — one budget per import run.
    error_budget: Mutex<ErrorBudget>,
    stop: Mutex<StopRequest>,
    /// Whether a [`Self::run_import`] is in flight. Read and cleared under
    /// the `stop` lock, so a Pause either reaches the running loop — which
    /// drains its window and then pauses the nest itself — or finds no loop
    /// and pauses the nest directly; never neither.
    run_active: Mutex<bool>,
    /// Set by an accepted [`MailImportAction::SkipRest`]: the runs that
    /// follow walk the rest of the scope recording skips instead of
    /// importing, through a Pause and Resume too, until the session ends.
    skip_rest: Mutex<bool>,
    /// The Microsoft sign-in's HTTPS seam; [`NoOAuthHttp`] until the shell
    /// supplies one.
    oauth: Arc<dyn ImportOAuthHttp>,
    oauth_available: bool,
    /// The identity platform's endpoints and the effective registration's
    /// client ID — `MicrosoftRegistration::effective` over the admin's pair
    /// [`MailImportNest::microsoft_registration`] answers, re-read at every
    /// [`Self::refresh`]. PRIVATE state: the UI has nothing to show, so it
    /// never reaches the rendered snapshot.
    microsoft: Mutex<OAuthClient>,
    /// A test's scripted far end, which [`Self::refresh`] must not replace.
    #[cfg(test)]
    microsoft_pinned: bool,
    signin: Mutex<SignInState>,
    /// Where the account remembers the source (§ Credential handling);
    /// `None` until the shell supplies one ([`Self::with_source_memory`]) —
    /// the wizard then remembers nothing.
    memory: Option<Arc<dyn MailImportSourceStore>>,
    /// The record [`Self::refresh`] last read through `memory`, kept for the
    /// cold adoption's seed. PRIVATE state: the snapshot carries only
    /// whether one exists.
    remembered: Mutex<Option<RememberedImportSource>>,
    /// Bumped on every write of the held session view. [`Self::refresh`]
    /// reads it before its listing and drops a listing the view moved past
    /// while it was in flight — a stale read must never paint an older state
    /// over a newer one.
    view_epoch: std::sync::atomic::AtomicU64,
}

impl MailImportMachine {
    pub fn new(
        nest: Arc<dyn MailImportNest>,
        source: Arc<dyn ImportSourceNest>,
        clock: Arc<dyn RetryClock>,
    ) -> Self {
        Self {
            nest,
            source,
            clock,
            inner: Mutex::new(MailImportSnapshot::empty()),
            session_id: Mutex::new(None),
            cursors: Mutex::new(HashMap::new()),
            error_budget: Mutex::new(ErrorBudget::default()),
            stop: Mutex::new(StopRequest::None),
            run_active: Mutex::new(false),
            skip_rest: Mutex::new(false),
            oauth: Arc::new(NoOAuthHttp),
            oauth_available: false,
            microsoft: Mutex::new(MicrosoftRegistration::project().client()),
            #[cfg(test)]
            microsoft_pinned: false,
            signin: Mutex::new(SignInState::default()),
            memory: None,
            remembered: Mutex::new(None),
            view_epoch: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Give the machine the account's store for the remembered source
    /// (`mailbox-migration.md` § Credential handling): every refresh reads it
    /// to prefill the Source step, every successful connection writes it, and
    /// [`MailImportAction::ForgetSource`] deletes it.
    pub fn with_source_memory(mut self, memory: Arc<dyn MailImportSourceStore>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Read the remembered source, keeping it for the cold adoption. A read
    /// that fails — no account runtime up yet, a store refusal — remembers
    /// nothing: the fields stay as they are, and the next refresh reads
    /// again.
    async fn load_remembered(&self) -> Option<RememberedImportSource> {
        let memory = self.memory.as_ref()?;
        let remembered = match memory.load().await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(
                    target: "fauna_mail_settings",
                    "the remembered import source could not be read: {e}"
                );
                return None;
            }
        };
        *self.remembered.lock().expect("remembered mutex") = remembered.clone();
        remembered
    }

    /// Remember the source the snapshot just connected to. A refused write
    /// costs the user only the next visit's prefill, so it is logged, never
    /// put between them and the import.
    async fn remember_source(&self) {
        let Some(memory) = self.memory.as_ref() else {
            return;
        };
        let record = self
            .inner
            .lock()
            .expect("snapshot mutex")
            .remembered_source();
        match memory.save(Some(record.clone())).await {
            Ok(()) => {
                *self.remembered.lock().expect("remembered mutex") = Some(record);
                self.inner.lock().expect("snapshot mutex").source_remembered = true;
            }
            Err(e) => tracing::warn!(
                target: "fauna_mail_settings",
                "the import source could not be remembered: {e}"
            ),
        }
    }

    /// `ForgetSource`: delete the account's record and return the Source
    /// step's fields to a first visit's.
    async fn forget_source(&self) -> Result<(), DispatchError> {
        let Some(memory) = self.memory.as_ref() else {
            return Err(DispatchError::InvalidState(
                "this app remembers no import source".into(),
            ));
        };
        self.set_status(ImportStatus::Working);
        memory.save(None).await?;
        *self.remembered.lock().expect("remembered mutex") = None;
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.source_remembered = false;
        if snap.step == ImportStep::Source {
            snap.clear_source_fields();
        }
        snap.status = ImportStatus::Idle;
        Ok(())
    }

    /// Give the machine the platform's HTTPS seam for the Microsoft sign-in
    /// (every native shell; web waits for the relay).
    pub fn with_oauth_http(mut self, oauth: Arc<dyn ImportOAuthHttp>) -> Self {
        self.oauth = oauth;
        self.oauth_available = true;
        self.refresh_signin_availability();
        self
    }

    /// Point the sign-in at another identity far end — the tests' scripted
    /// one. Production goes through `MicrosoftRegistration::effective`.
    #[cfg(test)]
    fn with_microsoft_client(mut self, client: OAuthClient) -> Self {
        *self.microsoft.get_mut().expect("microsoft mutex") = client;
        self.microsoft_pinned = true;
        self.refresh_signin_availability();
        self
    }

    /// Adopt the registration the nest reports (`None` — the project's):
    /// § Microsoft sign-in, *An admin-supplied registration*. An admin-set
    /// registration makes the button live even while the project's constant
    /// is unfilled.
    fn adopt_microsoft_registration(&self, admin: Option<MicrosoftRegistration>) {
        #[cfg(test)]
        if self.microsoft_pinned {
            return;
        }
        *self.microsoft.lock().expect("microsoft mutex") =
            MicrosoftRegistration::effective(admin.as_ref()).client();
        self.refresh_signin_availability();
    }

    fn microsoft_client(&self) -> OAuthClient {
        self.microsoft.lock().expect("microsoft mutex").clone()
    }

    /// The client the running sign-in was started with, else the current one.
    fn signin_client(&self) -> OAuthClient {
        self.signin
            .lock()
            .expect("sign-in mutex")
            .client
            .clone()
            .unwrap_or_else(|| self.microsoft_client())
    }

    fn refresh_signin_availability(&self) {
        let available = self.oauth_available && !self.microsoft_client().client_id.is_empty();
        self.inner
            .lock()
            .expect("snapshot mutex")
            .microsoft_signin_available = available;
    }

    fn set_signin_view(&self, view: Option<MicrosoftSignInView>) {
        self.inner.lock().expect("snapshot mutex").microsoft_signin = view;
    }

    fn set_signin_state(&self, state: MicrosoftSignInState) {
        if let Some(view) = self
            .inner
            .lock()
            .expect("snapshot mutex")
            .microsoft_signin
            .as_mut()
        {
            view.state = state;
        }
    }

    /// § Credential handling: the tokens end with the import — at Done,
    /// Cancel and an error. Dropping them zeroizes them.
    fn forget_microsoft_token(&self) {
        let mut signin = self.signin.lock().expect("sign-in mutex");
        signin.token = None;
        signin.flow = None;
        signin.client = None;
        signin.reconnect_pending = false;
    }

    /// Whether a Microsoft token is held — tier_1's witness that it ends.
    #[cfg(test)]
    fn holds_microsoft_token(&self) -> bool {
        self.signin.lock().expect("sign-in mutex").token.is_some()
    }

    fn set_status(&self, status: ImportStatus) {
        self.inner.lock().expect("snapshot mutex").status = status;
    }

    fn store_session(&self, session: ImportSessionView) {
        self.view_epoch
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.session_id.lock().expect("session id mutex") = Some(session.session_id.clone());
        self.inner
            .lock()
            .expect("snapshot mutex")
            .apply_session(session);
    }

    fn current_session_id(&self) -> Result<String, DispatchError> {
        self.session_id
            .lock()
            .expect("session id mutex")
            .clone()
            .ok_or_else(|| DispatchError::InvalidState("no active import session".into()))
    }

    /// Page load / resume, and the re-read an import event nudges
    /// (§ A failed import is a record the wizard shows until the user moves
    /// on). Shows the newest live session (`running` / `paused`, the states
    /// § Resume protocol resumes) on its Progress screen; with none, the
    /// newest `errored` one — the failure as a record, until it is dismissed
    /// or another import starts. A `completed` session is never adopted: it
    /// would hold every later visit on Done, with no Source step to start the
    /// next import from (the export twin does adopt `completed` — a finished
    /// export still has a download to collect). A `cancelled` one is shown
    /// only by the app session that cancelled it, which still holds it.
    async fn refresh(&self) -> Result<(), DispatchError> {
        self.set_status(ImportStatus::Loading);
        let remembered = self.load_remembered().await;
        let epoch = self.view_epoch.load(std::sync::atomic::Ordering::SeqCst);
        let sessions = self.nest.list_sessions().await?;
        let registration = self.nest.microsoft_registration().await?;
        self.adopt_microsoft_registration(registration);
        self.set_status(ImportStatus::Idle);
        if self.memory.is_some() {
            self.inner.lock().expect("snapshot mutex").source_remembered =
                self.remembered.lock().expect("remembered mutex").is_some();
        }
        if self.view_epoch.load(std::sync::atomic::Ordering::SeqCst) != epoch {
            // The held view moved while the listing was on its way — the run
            // paused it, a control answered — so the listing is older than
            // what is painted, and the newer state stands.
            return Ok(());
        }
        if *self.run_active.lock().expect("run mutex") {
            // A machine holding a run ignores the nudge: its loop is the
            // session view's writer, the import events are its own, and
            // whatever another device did to the session reaches it as a
            // refused call on its next send.
            return Ok(());
        }
        let held = self.session_id.lock().expect("session id mutex").clone();
        // The session this app finished or cancelled itself stays up for
        // review — step 5's counts and skip log, step 6's Done — however the
        // listing ranks it, unless a live import has since begun.
        let held_row = held
            .as_deref()
            .and_then(|id| sessions.iter().find(|s| s.session_id == id));
        let any_live = sessions.iter().any(|s| {
            matches!(
                s.state,
                ImportSessionState::Running | ImportSessionState::Paused
            )
        });
        if let Some(row) = held_row
            && matches!(
                row.state,
                ImportSessionState::Completed | ImportSessionState::Cancelled
            )
            && !any_live
        {
            self.store_session(row.clone());
            return Ok(());
        }
        let walking =
            held.is_none() && self.inner.lock().expect("snapshot mutex").step != ImportStep::Source;
        if walking {
            // Part-way through its own walk (Scope, Confirm) the wizard is the
            // person's: a refresh does not pull it onto another device's
            // import. They meet that import at Start — the per-source lock —
            // or on their next visit, which opens on the Source step.
            return Ok(());
        }
        if let Some(shown) = session_to_show(&sessions) {
            // A session this machine did not start or adopt already is one a
            // restarted app — or another of the person's devices — finds
            // (§ Resume protocol steps 1–3): it holds no run state for it, so
            // the walk is taken from the row — the source, the scope and each
            // mailbox's cursor — and Resume asks for the credential again.
            if held.as_deref() != Some(shown.session_id.as_str()) {
                self.adopt_cold(&shown, remembered.as_ref());
            }
            self.store_session(shown);
        } else if held.is_some() {
            // The session this machine holds is no longer listed as anything
            // to show — dismissed or swept from another of the person's
            // devices, or expired. Keeping it would paint a record that no
            // longer exists; only a HELD session is dropped, so a wizard mid-way
            // through Source or Scope keeps its choices across a refresh.
            self.clear_session();
        } else if let Some(remembered) = &remembered {
            // § Credential handling: the source the account remembers fills
            // a Source step the user has not started typing into — never one
            // they have, and never a later step.
            let mut snap = self.inner.lock().expect("snapshot mutex");
            if snap.step == ImportStep::Source && snap.username.is_empty() {
                snap.prefill_from(remembered);
            }
        }
        Ok(())
    }

    /// Forget the held session and every run's state, back to the Source step.
    fn clear_session(&self) {
        self.view_epoch
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.session_id.lock().expect("session id mutex") = None;
        self.cursors.lock().expect("cursors mutex").clear();
        *self.error_budget.lock().expect("error budget mutex") = ErrorBudget::default();
        *self.stop.lock().expect("stop mutex") = StopRequest::None;
        *self.skip_rest.lock().expect("skip mutex") = false;
        self.inner.lock().expect("snapshot mutex").reset_to_source();
    }

    /// [`Self::refresh`]'s cold adoption: seed the snapshot's source and scope
    /// and the run's cursors from the row (§ Resume protocol step 3 — each
    /// mailbox continues after its `last_processed_source_uid`, and step 4's
    /// UIDVALIDITY check runs against the recorded validity). A cursor for a
    /// mailbox outside the scope is ignored.
    fn adopt_cold(&self, s: &ImportSessionView, remembered: Option<&RememberedImportSource>) {
        {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            snap.adopt_session_source(s, remembered);
            // Another session's lines and rows are not this one's.
            snap.error_log.clear();
            snap.mailbox_progress.clear();
            snap.rebuilt_mailboxes.clear();
            snap.pause_reason = None;
        }
        *self.cursors.lock().expect("cursors mutex") = s
            .cursors
            .iter()
            .filter(|c| s.scope.contains(&c.mailbox))
            .map(|c| {
                (
                    c.mailbox.clone(),
                    MailboxCursor {
                        last_processed_source_uid: c.last_processed_source_uid,
                        source_uid_validity: c.source_uid_validity,
                    },
                )
            })
            .collect();
        *self.error_budget.lock().expect("error budget mutex") = ErrorBudget::default();
        *self.stop.lock().expect("stop mutex") = StopRequest::None;
    }

    /// A Resume in an app that adopted the session cold (§ Resume protocol
    /// step 2): open the source again with the credential the person has just
    /// given — the password on the snapshot, or the Microsoft sign-in's token
    /// on the Outlook source — before anything else happens. Without one the
    /// Resume is refused with a sentence and nothing reaches the source or the
    /// nest; a refused connection says why, keeps the typed password for a
    /// retry (§ Credential handling), and resumes nothing.
    async fn reconnect_for_resume(&self) -> Result<(), DispatchError> {
        use fauna_i18n::strings::mail_import as s;
        let params = {
            let snap = self.inner.lock().expect("snapshot mutex");
            SourceConnectParams {
                kind: snap.source_kind,
                host: snap.host.clone(),
                port: snap.port,
                tls_mode: snap.tls_mode,
                username: snap.username.clone(),
                auth: SourceAuth::Password(snap.password.clone()),
            }
        };
        let kind = params.kind;
        let mut params = params;
        if kind == ImportSourceKind::Outlook {
            let token = self
                .signin
                .lock()
                .expect("sign-in mutex")
                .token
                .as_ref()
                .map(HeldToken::auth);
            let Some(auth) = token else {
                return Err(DispatchError::InvalidState(
                    s::RESUME_NEEDS_MICROSOFT.into(),
                ));
            };
            params.auth = auth;
        } else if matches!(&params.auth, SourceAuth::Password(p) if p.as_str().is_empty()) {
            return Err(DispatchError::InvalidState(s::RESUME_NEEDS_PASSWORD.into()));
        }
        self.set_status(ImportStatus::Working);
        if let Err(e) = self.source.connect(params).await {
            if !self.source.can_ever_connect() {
                self.inner.lock().expect("snapshot mutex").password = SecretString::default();
            }
            if kind == ImportSourceKind::Outlook {
                self.forget_microsoft_token();
            }
            tracing::warn!(target: "fauna_mail_settings", "import resume reconnect failed: {e}");
            self.set_status(ImportStatus::Idle);
            return Err(DispatchError::InvalidState(connect_failure_sentence(
                kind, e.failure,
            )));
        }
        {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            snap.password = SecretString::default();
            snap.microsoft_signin = None;
            snap.resume_needs_credentials = false;
        }
        // The port and connection security the person may have just corrected
        // are what the next resume, here or on another device, re-uses.
        self.remember_source().await;
        Ok(())
    }

    /// Step 1→3. § Credential handling: the password is consumed here and
    /// cleared from the snapshot on success; a failed connect leaves every
    /// field (password included) exactly as the user typed it, so "retry
    /// without re-entering credentials" (§ Wizard steps step 2) holds —
    /// *unless* [`ImportSourceNest::can_ever_connect`] says this seam can
    /// never succeed, in which case retry-without-retyping is not a benefit
    /// this platform can ever deliver, so the failed-connect password is
    /// cleared too (§ Credential handling's retain-on-failure exception).
    ///
    /// The Outlook source connects with the Microsoft token a sign-in granted
    /// (§ Microsoft sign-in — the device-code grant) and never with a
    /// password; without one it is refused with a sentence. A failed
    /// connection forgets the token — the person signs in again.
    async fn connect(&self) -> Result<(), DispatchError> {
        self.set_status(ImportStatus::Working);
        let (params_kind, mut params) = {
            let snap = self.inner.lock().expect("snapshot mutex");
            (
                snap.source_kind,
                SourceConnectParams {
                    kind: snap.source_kind,
                    host: snap.host.clone(),
                    port: snap.port,
                    tls_mode: snap.tls_mode,
                    username: snap.username.clone(),
                    auth: SourceAuth::Password(snap.password.clone()),
                },
            )
        };
        let outlook = params_kind == ImportSourceKind::Outlook;
        if outlook {
            let auth = self
                .signin
                .lock()
                .expect("sign-in mutex")
                .token
                .as_ref()
                .map(HeldToken::auth);
            let Some(auth) = auth else {
                self.set_status(ImportStatus::Idle);
                return Err(DispatchError::InvalidState(
                    fauna_i18n::strings::mail_import::MICROSOFT_SIGN_IN_REQUIRED.into(),
                ));
            };
            params.auth = auth;
        }
        if let Err(e) = self.source.connect(params).await {
            if !self.source.can_ever_connect() {
                self.inner.lock().expect("snapshot mutex").password = SecretString::default();
            }
            if outlook {
                self.forget_microsoft_token();
                self.set_signin_state(MicrosoftSignInState::Failed);
            }
            // The source's own words go to the app's log; the wizard says
            // what went wrong in plain words (§ Wizard steps step 2).
            tracing::warn!(target: "fauna_mail_settings", "import source connect failed: {e}");
            return Err(DispatchError::InvalidState(connect_failure_sentence(
                params_kind,
                e.failure,
            )));
        }
        let mailboxes = self.source.list_source_mailboxes().await?;
        self.remember_source().await;
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.mailboxes = mailboxes
            .into_iter()
            .filter(|m| m.selectable)
            .map(|m| SourceMailboxOption {
                selected: default_selected(&m.name, m.special_use),
                name: m.name,
                message_count: 0,
                byte_total: None,
            })
            .collect();
        snap.password = SecretString::default();
        snap.microsoft_signin = None;
        snap.step = ImportStep::Scope;
        snap.status = ImportStatus::Idle;
        Ok(())
    }

    /// `BeginMicrosoftSignIn`: ask the identity platform for a code and put
    /// it on the snapshot. Any token from an earlier sign-in is forgotten
    /// first — one sign-in, one account.
    async fn begin_microsoft_sign_in(&self) -> Result<(), DispatchError> {
        use fauna_i18n::strings::mail_import as s;
        let (kind, username, available) = {
            let snap = self.inner.lock().expect("snapshot mutex");
            (
                snap.source_kind,
                snap.username.trim().to_string(),
                snap.microsoft_signin_available,
            )
        };
        if kind != ImportSourceKind::Outlook {
            return Err(DispatchError::InvalidState(
                "Microsoft sign-in is for the Outlook source".into(),
            ));
        }
        if !available {
            return Err(DispatchError::InvalidState(s::MICROSOFT_UNAVAILABLE.into()));
        }
        if username.is_empty() {
            return Err(DispatchError::InvalidState(
                s::MICROSOFT_NEEDS_ADDRESS.into(),
            ));
        }
        let client = self.microsoft_client();
        {
            let mut signin = self.signin.lock().expect("sign-in mutex");
            *signin = SignInState {
                client: Some(client.clone()),
                ..SignInState::default()
            };
        }
        self.set_status(ImportStatus::Working);
        self.set_signin_view(Some(MicrosoftSignInView {
            user_code: String::new(),
            verification_uri: String::new(),
            expires_at_ms: 0,
            state: MicrosoftSignInState::Requesting,
        }));
        let started = DeviceFlow::start(
            &DynOAuthHttp(self.oauth.as_ref()),
            &client,
            self.clock.now_ms(),
        )
        .await;
        self.set_status(ImportStatus::Idle);
        match started {
            Ok(flow) => {
                let code = flow.authorization();
                self.set_signin_view(Some(MicrosoftSignInView {
                    user_code: code.user_code.clone(),
                    verification_uri: code.verification_uri.clone(),
                    expires_at_ms: code.expires_at_ms,
                    state: MicrosoftSignInState::Waiting,
                }));
                self.signin.lock().expect("sign-in mutex").flow = Some(flow);
                Ok(())
            }
            Err(e) => {
                tracing::warn!(target: "fauna_mail_settings", "Microsoft sign-in did not start: {e}");
                self.set_signin_state(MicrosoftSignInState::Failed);
                let sentence = match e {
                    oauth_device::OAuthError::Unavailable => s::MICROSOFT_UNAVAILABLE,
                    _ => s::MICROSOFT_FAILED,
                };
                Err(DispatchError::InvalidState(sentence.into()))
            }
        }
    }

    /// `CancelMicrosoftSignIn`: stop the polling at its next turn and forget
    /// the code and any token.
    fn cancel_microsoft_sign_in(&self) {
        {
            let mut signin = self.signin.lock().expect("sign-in mutex");
            signin.cancelled = true;
            signin.flow = None;
            signin.token = None;
        }
        self.set_signin_view(None);
    }

    /// The sentence `error-message` shows when a sign-in ends without a
    /// grant.
    fn signin_ended(&self, state: MicrosoftSignInState, sentence: String) -> DispatchError {
        self.set_signin_state(state);
        let mut snap = self.inner.lock().expect("snapshot mutex");
        crate::state::set_snapshot_error(&mut snap.error, sentence.clone());
        snap.status = ImportStatus::Idle;
        DispatchError::InvalidState(sentence)
    }

    /// The polling half, after a `BeginMicrosoftSignIn`: poll until the
    /// person finishes, then connect with the token. Ends the sign-in with a
    /// plain sentence on decline, expiry, a tenant that needs its
    /// administrator's consent, or any other failure — the Source step stays
    /// as it was (§ Failure handling). A cancelled sign-in ends quietly.
    async fn run_microsoft_sign_in_inner(&self) -> Result<(), DispatchError> {
        use fauna_i18n::strings::mail_import as s;
        let flow = self.signin.lock().expect("sign-in mutex").flow.take();
        let Some(mut flow) = flow else {
            return Ok(());
        };
        let outcome = flow
            .run(
                &DynOAuthHttp(self.oauth.as_ref()),
                &DynClock(self.clock.as_ref()),
                || self.signin.lock().expect("sign-in mutex").cancelled,
            )
            .await;
        match outcome {
            FlowOutcome::Granted(grant) => {
                let user = self
                    .inner
                    .lock()
                    .expect("snapshot mutex")
                    .username
                    .trim()
                    .to_string();
                {
                    let mut signin = self.signin.lock().expect("sign-in mutex");
                    if signin.cancelled {
                        return Ok(());
                    }
                    signin.token = Some(HeldToken {
                        user,
                        access_token: grant.access_token,
                        refresh_token: grant.refresh_token,
                        expires_at_ms: grant.expires_at_ms,
                    });
                }
                // Signing in to resume an adopted session (§ Resume protocol
                // step 2): the token is held for the person's Resume, which
                // reconnects with it — `connect` would list the mailboxes and
                // leave the Progress step for Scope.
                if self
                    .inner
                    .lock()
                    .expect("snapshot mutex")
                    .resume_needs_credentials
                {
                    self.set_signin_view(None);
                    return Ok(());
                }
                self.set_signin_state(MicrosoftSignInState::Connecting);
                self.connect().await
            }
            FlowOutcome::Cancelled => Ok(()),
            FlowOutcome::Declined => {
                Err(self.signin_ended(MicrosoftSignInState::Declined, s::MICROSOFT_DECLINED.into()))
            }
            FlowOutcome::Expired => {
                Err(self.signin_ended(MicrosoftSignInState::Expired, s::MICROSOFT_EXPIRED.into()))
            }
            FlowOutcome::AdminConsentRequired => Err(self.signin_ended(
                MicrosoftSignInState::AdminConsentRequired,
                s::microsoft_admin_consent_fmt(&admin_consent_url(&self.signin_client())),
            )),
            FlowOutcome::Failed(detail) => {
                tracing::warn!(target: "fauna_mail_settings", "Microsoft sign-in failed: {detail}");
                Err(self.signin_ended(MicrosoftSignInState::Failed, s::MICROSOFT_FAILED.into()))
            }
        }
    }

    /// § Microsoft sign-in, *Lifetime and refresh*: on a reconnect, an access
    /// token with under five minutes left is traded once for a fresh one,
    /// which the source seam signs in with the next time it opens the
    /// connection. Nothing to do for a password source.
    async fn refresh_for_reconnect(&self) -> RefreshOutcome {
        let refresh_token = {
            let signin = self.signin.lock().expect("sign-in mutex");
            let Some(token) = signin.token.as_ref() else {
                return RefreshOutcome::Fresh;
            };
            if token.expires_at_ms.saturating_sub(self.clock.now_ms()) >= TOKEN_REFRESH_MARGIN_MS {
                return RefreshOutcome::Fresh;
            }
            match token.refresh_token.clone() {
                Some(t) => t,
                None => return RefreshOutcome::Refused,
            }
        };
        let refreshed = oauth_device::refresh(
            &DynOAuthHttp(self.oauth.as_ref()),
            &self.signin_client(),
            &refresh_token,
            self.clock.now_ms(),
        )
        .await;
        match refreshed {
            Ok(TokenGrant {
                access_token,
                refresh_token,
                expires_at_ms,
            }) => {
                let auth = {
                    let mut signin = self.signin.lock().expect("sign-in mutex");
                    let Some(token) = signin.token.as_mut() else {
                        return RefreshOutcome::Fresh;
                    };
                    token.access_token = access_token;
                    if refresh_token.is_some() {
                        token.refresh_token = refresh_token;
                    }
                    token.expires_at_ms = expires_at_ms;
                    token.auth()
                };
                self.source.replace_auth(auth).await;
                RefreshOutcome::Fresh
            }
            Err(oauth_device::OAuthError::Http(detail)) => {
                tracing::warn!(target: "fauna_mail_settings", "Microsoft token refresh unreachable: {detail}");
                RefreshOutcome::Unreachable
            }
            Err(e) => {
                tracing::warn!(target: "fauna_mail_settings", "Microsoft token refresh refused: {e}");
                RefreshOutcome::Refused
            }
        }
    }

    /// The Resume reconnect's refresh, taken once at the start of the run
    /// that follows a Resume. `false` when it was refused.
    async fn refresh_after_resume(&self) -> bool {
        let pending =
            std::mem::take(&mut self.signin.lock().expect("sign-in mutex").reconnect_pending);
        !pending || !matches!(self.refresh_for_reconnect().await, RefreshOutcome::Refused)
    }

    /// Whether a refresh was refused mid-run; taken once.
    fn take_sign_in_again(&self) -> bool {
        std::mem::take(&mut self.signin.lock().expect("sign-in mutex").sign_in_again)
    }

    /// The session's end when the source refused the kept credential on a
    /// sign-in part-way through the run (§ Failure handling → *Source-side
    /// errors*): the Outlook source is asked to sign in with Microsoft again,
    /// every other source for new credentials.
    fn sign_in_refused_sentence(&self) -> String {
        let kind = self.inner.lock().expect("snapshot mutex").source_kind;
        match kind {
            ImportSourceKind::Outlook => sign_in_again(),
            _ => fauna_i18n::strings::mail_import::PASSWORD_SESSION_SIGN_IN_AGAIN.to_string(),
        }
    }

    /// Scope → Confirm: ask the source how much each selected mailbox holds
    /// for this import (§ Wizard steps step 4's "total messages + estimated
    /// bytes"), then show the summary. Run here, for the selected mailboxes
    /// only, rather than for every listed one at `Connect`: the scope step's
    /// since date decides the count, and a source without `STATUS=SIZE` is
    /// sized by enumerating a mailbox, which over Gmail's labels — the same
    /// mail listed under several — would multiply work the user never asked
    /// for. A date nobody can read is refused here, on the step that holds
    /// the field. A connection fault keeps the wizard on Scope with the
    /// error; a mailbox the source refuses to be asked about is left
    /// unestimated (its count `0`, no size), which leaves the summary
    /// without a size rather than refusing the import.
    async fn next(&self) -> Result<(), DispatchError> {
        self.set_status(ImportStatus::Working);
        let (selected, date_from) = {
            let snap = self.inner.lock().expect("snapshot mutex");
            let selected: Vec<String> = snap
                .mailboxes
                .iter()
                .filter(|m| m.selected)
                .map(|m| m.name.clone())
                .collect();
            (selected, snap.date_from.clone())
        };
        let floor = ImportDateFloor::parse(&date_from)?;
        let mut estimates = Vec::with_capacity(selected.len());
        for mailbox in selected {
            let estimate = match self.source.estimate_mailbox(&mailbox, floor.since).await {
                Ok(e) => Some(e),
                Err(NestError::Rejected(reason)) => {
                    tracing::warn!(
                        target: "fauna_mail_settings",
                        "import estimate refused for a mailbox: {reason}"
                    );
                    None
                }
                Err(e) => return Err(e.into()),
            };
            estimates.push((mailbox, estimate));
        }
        let mut snap = self.inner.lock().expect("snapshot mutex");
        for (mailbox, estimate) in estimates {
            if let Some(m) = snap.mailboxes.iter_mut().find(|m| m.name == mailbox) {
                m.message_count = estimate.map_or(0, |e| e.messages);
                m.byte_total = estimate.and_then(|e| e.bytes);
            }
        }
        snap.step = ImportStep::Confirm;
        snap.status = ImportStatus::Idle;
        Ok(())
    }

    async fn start(&self) -> Result<(), DispatchError> {
        self.set_status(ImportStatus::Working);
        let (descriptor, total_count, scope, date_from, skip_dedup) = {
            let snap = self.inner.lock().expect("snapshot mutex");
            snap.start_args()
        };
        // Resolved BEFORE the session opens: a date nobody can read must
        // refuse the Start, not surface once a row already holds the source
        // lock (§ Architectural rules) and the user is on the Progress screen.
        let refused = ImportDateFloor::parse(&date_from).err();
        if let Some(e) = refused {
            self.set_status(ImportStatus::Idle);
            return Err(e);
        }
        let session = self
            .nest
            .start_session(descriptor, total_count, scope, date_from, skip_dedup)
            .await
            .map_err(DispatchError::refused_control)?;
        self.store_session(session);
        // A fresh run: no mailbox has been touched yet under this session.
        {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            snap.mailbox_progress = snap
                .mailboxes
                .iter()
                .filter(|m| m.selected)
                .map(|m| ImportMailboxProgress {
                    name: m.name.clone(),
                    imported: 0,
                    skipped: 0,
                    errored: 0,
                    total: u64::from(m.message_count),
                })
                .collect();
        }
        *self.cursors.lock().expect("cursors mutex") = HashMap::new();
        self.inner
            .lock()
            .expect("snapshot mutex")
            .rebuilt_mailboxes
            .clear();
        *self.error_budget.lock().expect("error budget mutex") = ErrorBudget::default();
        *self.stop.lock().expect("stop mutex") = StopRequest::None;
        *self.skip_rest.lock().expect("skip mutex") = false;
        self.set_status(ImportStatus::Idle);
        Ok(())
    }

    /// § UX shape step 5: `run_import`'s loop polls [`StopRequest`] between
    /// fetch windows and drains the in-flight one before actually stopping.
    /// With a run in flight this method only records the request — the loop
    /// pauses the nest-side session once its window is handed over, because
    /// the nest takes messages only into a running session; with none, it
    /// pauses the session here. It never blocks on the loop noticing.
    /// Pause, Resume and Cancel move a `running` / `paused` session only. On
    /// a failed or cancelled one they are not offered (§ A failed import is a
    /// record the wizard shows until the user moves on), and should an app
    /// still send one it is refused here with a sentence, before the nest is
    /// asked — the nest would refuse it too, naming nothing the user can act
    /// on.
    fn refuse_on_terminal_session(&self) -> Result<(), DispatchError> {
        use fauna_i18n::strings::mail_import as s;
        match self.inner.lock().expect("snapshot mutex").session_state {
            Some(ImportSessionState::Errored) => {
                Err(DispatchError::InvalidState(s::FAILED_IMPORT_CONTROL.into()))
            }
            Some(ImportSessionState::Cancelled) => Err(DispatchError::InvalidState(
                s::CANCELLED_IMPORT_CONTROL.into(),
            )),
            _ => Ok(()),
        }
    }

    /// Dismiss (`mail-import-dismiss-button`): delete a finished session's
    /// record and return to Source. Refused on a live one before the nest is
    /// asked; a record already gone — dismissed on another device — is the
    /// same outcome.
    async fn dismiss(&self) -> Result<(), DispatchError> {
        let state = self.inner.lock().expect("snapshot mutex").session_state;
        if matches!(
            state,
            Some(ImportSessionState::Running | ImportSessionState::Paused)
        ) {
            return Err(DispatchError::InvalidState(
                fauna_i18n::strings::mail_import::DISMISS_LIVE_IMPORT.into(),
            ));
        }
        let id = self.current_session_id()?;
        self.set_status(ImportStatus::Working);
        self.nest
            .discard_session(id)
            .await
            .map_err(DispatchError::refused_control)?;
        self.clear_session();
        Ok(())
    }

    async fn pause(&self) -> Result<(), DispatchError> {
        self.refuse_on_terminal_session()?;
        let id = self.current_session_id()?;
        {
            // A run in flight pauses the session itself, after handing its
            // window to the nest: the nest takes messages only into a running
            // session, so pausing it here first would refuse that drain.
            let mut stop = self.stop.lock().expect("stop mutex");
            if *self.run_active.lock().expect("run mutex") {
                *stop = StopRequest::Pause;
                return Ok(());
            }
        }
        self.set_status(ImportStatus::Working);
        let session = self
            .nest
            .pause_session(id)
            .await
            .map_err(DispatchError::refused_control)?;
        self.store_session(session);
        *self.stop.lock().expect("stop mutex") = StopRequest::Pause;
        self.set_status(ImportStatus::Idle);
        Ok(())
    }

    async fn resume(&self) -> Result<(), DispatchError> {
        self.refuse_on_terminal_session()?;
        let id = self.current_session_id()?;
        {
            // Resume before the running loop got to its pause (`pause`): the
            // nest session never stopped, so withdrawing the request is all.
            let mut stop = self.stop.lock().expect("stop mutex");
            if *self.run_active.lock().expect("run mutex") && *stop == StopRequest::Pause {
                *stop = StopRequest::None;
                return Ok(());
            }
        }
        let (cold, state) = {
            let snap = self.inner.lock().expect("snapshot mutex");
            (snap.resume_needs_credentials, snap.session_state)
        };
        if cold {
            self.reconnect_for_resume().await?;
        }
        self.set_status(ImportStatus::Working);
        // A `running` row adopted cold is one whose app stopped without
        // pausing it (it was closed, or crashed): the nest already holds it
        // running and refuses a resume, so the run simply starts here.
        if !(cold && state == Some(ImportSessionState::Running)) {
            let session = self
                .nest
                .resume_session(id)
                .await
                .map_err(DispatchError::refused_control)?;
            self.store_session(session);
        }
        self.inner.lock().expect("snapshot mutex").pause_reason = None;
        *self.stop.lock().expect("stop mutex") = StopRequest::None;
        // A Resume is a reconnect (§ Microsoft sign-in, *Lifetime and
        // refresh*): the run it starts refreshes a token near its end first.
        self.signin.lock().expect("sign-in mutex").reconnect_pending = true;
        self.set_status(ImportStatus::Idle);
        Ok(())
    }

    /// § Failure handling → *Nest-side errors*: "skip remaining" on the
    /// full-mailbox notice. The session is taken up again exactly as a
    /// Resume does (a cold one asks for its credentials first — the walk
    /// still enumerates the source), and the runs that follow record what is
    /// left as skipped rather than import it.
    async fn skip_the_rest(&self) -> Result<(), DispatchError> {
        let paused = self.inner.lock().expect("snapshot mutex").session_state
            == Some(ImportSessionState::Paused);
        if !paused {
            return Err(DispatchError::InvalidState(
                "only a paused import can skip the rest".into(),
            ));
        }
        self.resume().await?;
        *self.skip_rest.lock().expect("skip mutex") = true;
        Ok(())
    }

    /// § UX shape step 5: cancel keeps already-imported messages, so — unlike
    /// export's `Cancel` — the wizard stays on the Progress screen (via
    /// `apply_session`) rather than resetting to step 1. `run_import`'s loop
    /// aborts the in-flight fetch window rather than draining it (see
    /// [`StopRequest::Cancel`]).
    async fn cancel(&self) -> Result<(), DispatchError> {
        self.refuse_on_terminal_session()?;
        self.set_status(ImportStatus::Working);
        let id = self.current_session_id()?;
        let session = self
            .nest
            .cancel_session(id)
            .await
            .map_err(DispatchError::refused_control)?;
        self.store_session(session);
        self.forget_microsoft_token();
        *self.stop.lock().expect("stop mutex") = StopRequest::Cancel;
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.rebuilt_mailboxes.clear();
        snap.status = ImportStatus::Idle;
        Ok(())
    }

    /// `RestartRebuiltMailboxes` (§ Resume protocol step 4): resume the
    /// paused session with every rebuilt mailbox's cursor forgotten, so the
    /// run that follows walks each from its first message under the new
    /// UIDVALIDITY — the nest folds the new value over the row's cursor
    /// (`fold_mailbox`), and its dedup skips what already arrived. The
    /// cursors go only once the Resume has landed: a refused one leaves the
    /// offer standing.
    async fn restart_rebuilt_mailboxes(&self) -> Result<(), DispatchError> {
        let rebuilt = self
            .inner
            .lock()
            .expect("snapshot mutex")
            .rebuilt_mailboxes
            .clone();
        if rebuilt.is_empty() {
            return Err(DispatchError::InvalidState(
                "no rebuilt mailbox to restart".into(),
            ));
        }
        self.resume().await?;
        let mut cursors = self.cursors.lock().expect("cursors mutex");
        for mailbox in &rebuilt {
            cursors.remove(mailbox);
        }
        drop(cursors);
        self.inner
            .lock()
            .expect("snapshot mutex")
            .rebuilt_mailboxes
            .clear();
        Ok(())
    }

    /// The run found `mailbox` rebuilt (§ Resume protocol step 4): note it for
    /// the restart offer and log the warning, once however many runs meet it.
    fn note_rebuilt(&self, mailbox: &str) {
        let mut snap = self.inner.lock().expect("snapshot mutex");
        if snap.rebuilt_mailboxes.iter().any(|m| m == mailbox) {
            return;
        }
        snap.rebuilt_mailboxes.push(mailbox.to_string());
        snap.error_log
            .push(fauna_i18n::strings::mail_import::mailbox_rebuilt_fmt(
                mailbox,
            ));
    }

    fn push_error_log(&self, line: String) {
        self.inner
            .lock()
            .expect("snapshot mutex")
            .error_log
            .push(line);
    }

    /// § Failure handling: a TCP/TLS error mid-session gets up to 3 retries
    /// with exponential backoff (5 s, 30 s, 2 min) before the loop pauses the
    /// session. Only [`NestError::Transient`] is retried — a
    /// [`NestError::Rejected`] is the source telling us something true (bad
    /// creds, a rebuilt mailbox, …) and retrying would just ask the same
    /// question again.
    async fn retrying<T, F, Fut>(&self, mut op: F) -> Result<T, NestError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, NestError>>,
    {
        let mut attempt = 0usize;
        loop {
            match op().await {
                Ok(v) => return Ok(v),
                Err(NestError::Transient(_)) if attempt < RETRY_DELAYS_MS.len() => {
                    self.clock.sleep_ms(RETRY_DELAYS_MS[attempt]).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// [`Self::retrying`] for a source call: every retry after a dropped
    /// connection is a reconnect, so a Microsoft token near its end is
    /// refreshed first ([`Self::refresh_for_reconnect`]). A refused refresh
    /// stops the retries; the caller sees [`Self::take_sign_in_again`] and
    /// ends the session.
    async fn retrying_source<T, F, Fut>(&self, mut op: F) -> Result<T, NestError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, NestError>>,
    {
        let mut attempt = 0usize;
        loop {
            match op().await {
                Ok(v) => return Ok(v),
                Err(NestError::Transient(_)) if attempt < RETRY_DELAYS_MS.len() => {
                    self.clock.sleep_ms(RETRY_DELAYS_MS[attempt]).await;
                    attempt += 1;
                    if matches!(self.refresh_for_reconnect().await, RefreshOutcome::Refused) {
                        self.signin.lock().expect("sign-in mutex").sign_in_again = true;
                        return Err(NestError::Rejected(
                            "the Microsoft token refresh was refused".into(),
                        ));
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Report a session-fatal condition to nest, surface it on the snapshot's
    /// `error-message`, and return it as the `run_import` result.
    async fn fail_and_report(&self, session_id: String, reason: String) -> DispatchError {
        if let Ok(session) = self.nest.fail_session(session_id, reason.clone()).await {
            self.store_session(session);
        }
        let _ = self.source.logout().await;
        self.forget_microsoft_token();
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.note_failure_reason(reason.clone());
        crate::state::set_snapshot_error(&mut snap.error, reason.clone());
        snap.status = ImportStatus::Idle;
        DispatchError::InvalidState(reason)
    }

    /// Send one packed unit — retried on a dropped connection exactly as the
    /// source side is (§ Failure handling → *Nest-side errors*; a resend is
    /// safe, the nest's dedup skips what already landed) — and fold its
    /// outcomes into the rendered counters and the § Per-message error budget.
    /// Every lock here is taken and dropped synchronously, around the single
    /// `.await` — never held across it, or `run_import`'s future (spawned by
    /// `uniffi::export(async_runtime = "tokio")`) would stop being `Send`.
    async fn send_and_record(
        &self,
        session_id: &str,
        mailbox: &str,
        unit: ImportUnit,
    ) -> Result<SentUnit, NestError> {
        let uids = unit_uids(&unit);
        let mailboxes: Vec<String> = unit_items(&unit).map(|i| i.mailbox.clone()).collect();
        // A reply longer than the unit is the nest's bug, never a panic here.
        let line = |i: usize, reason: &str| {
            let mailbox = mailboxes.get(i).map_or("", String::as_str);
            skip_log_line(mailbox, uids.get(i).copied().unwrap_or(0), reason)
        };
        // The session's duplicates choice — fixed at Start and adopted from
        // the row by a wizard that found the session (§ Opt-out per session).
        let skip_dedup = self.inner.lock().expect("snapshot mutex").import_duplicates;
        let reply = self
            .retrying(|| {
                // No revision: the total the session opened with is already
                // the range's own count (`Self::next`).
                self.nest
                    .send_unit(session_id.to_string(), unit.clone(), skip_dedup, None)
            })
            .await?;
        let mut sent = SentUnit::default();
        let mut snap = self.inner.lock().expect("snapshot mutex");
        let mut budget = self.error_budget.lock().expect("error budget mutex");
        let (mut imported, mut skipped, mut errored) = (0u64, 0u64, 0u64);
        for (i, outcome) in reply.outcomes.into_iter().enumerate() {
            match outcome {
                SendOutcome::Imported => {
                    imported += 1;
                    sent.breached |= budget.record(false);
                }
                SendOutcome::Skipped { reason } => {
                    skipped += 1;
                    snap.error_log.push(line(i, &reason));
                    sent.breached |= budget.record(false);
                    if reason == IMPORT_SKIP_QUOTA_EXCEEDED && sent.quota_exceeded_at.is_none() {
                        sent.quota_exceeded_at = uids.get(i).copied();
                    }
                }
                SendOutcome::Errored { reason } => {
                    errored += 1;
                    snap.error_log.push(line(i, &reason));
                    sent.breached |= budget.record(true);
                }
            }
        }
        snap.imported_count += imported;
        snap.skipped_count += skipped;
        snap.errored_count += errored;
        if let Some(row) = snap.mailbox_row(mailbox) {
            row.imported += imported;
            row.skipped += skipped;
            row.errored += errored;
        }
        Ok(sent)
    }

    /// The drive loop stopping on its own (§ Failure handling): the session
    /// moves to `paused` — never `errored`, which nothing can resume — with
    /// the reason on `error-message` and the cursor kept, and the existing
    /// Resume takes it up again. What actually went wrong goes to the log.
    ///
    /// When the nest is the thing that is down, its pause cannot land either:
    /// the row then stays `running` with no loop behind it, which the
    /// progress step still shows with its reason, and the person's own Pause
    /// then Resume (once the nest answers) restarts the walk — never a state
    /// a client cannot leave.
    async fn pause_on_its_own(
        &self,
        session_id: String,
        reason: ImportPauseReason,
        detail: &str,
    ) -> Result<(), DispatchError> {
        tracing::warn!(target: "fauna_mail_settings", "import paused ({reason:?}): {detail}");
        if let Ok(session) = self.nest.pause_session(session_id).await {
            self.store_session(session);
        }
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.pause_reason = Some(reason);
        snap.error = Some(pause_reason_sentence(reason).to_string());
        snap.status = ImportStatus::Idle;
        Ok(())
    }

    /// [`Self::send_and_record`] for the drive loop: a breach or a full
    /// mailbox is folded into `breached` / `halt`, and a connection that stays
    /// down through every retry halts at the unit's first message. Only a
    /// refusal the nest means (`Rejected`) leaves the loop as an error.
    async fn send_unit_or_halt(
        &self,
        session_id: &str,
        mailbox: &str,
        unit: ImportUnit,
        breached: &mut bool,
        halt: &mut Option<(u32, ImportPauseReason, String)>,
    ) -> Result<(), DispatchError> {
        let first = unit_uids(&unit).into_iter().min().unwrap_or(0);
        match self.send_and_record(session_id, mailbox, unit).await {
            Ok(sent) => {
                *breached |= sent.breached;
                if let Some(uid) = sent.quota_exceeded_at {
                    *halt = Some((
                        uid,
                        ImportPauseReason::MailboxFull,
                        format!("the nest answered uid {uid} {IMPORT_SKIP_QUOTA_EXCEEDED}"),
                    ));
                }
                Ok(())
            }
            Err(NestError::Transient(detail)) => {
                *halt = Some((first, ImportPauseReason::ConnectionLost, detail));
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// One message's source-fetch failure (never reaches nest — no
    /// `import_message` call was ever made for it): logs it and records it
    /// against the error budget. Returns `true` on breach.
    fn record_source_fetch_failure(&self, mailbox: &str, uid: u32, reason: String) -> bool {
        self.push_error_log(skip_log_line(mailbox, uid, &reason));
        self.error_budget
            .lock()
            .expect("error budget mutex")
            .record(true)
    }

    async fn run_import_inner(&self) -> Result<(), DispatchError> {
        let session_id = self.current_session_id()?;
        // Both halves of the run's scope are re-read from the snapshot on
        // every call, exactly as the mailbox selection always has been: this
        // machine parks no run state across a Pause, so the snapshot IS the
        // scope — which is what makes a Resume re-apply the same floor
        // (§ Resume protocol) with no second copy to keep in step.
        let (mailboxes, date_from, max_size_bytes): (Vec<String>, String, u64) = {
            let snap = self.inner.lock().expect("snapshot mutex");
            (
                snap.mailboxes
                    .iter()
                    .filter(|m| m.selected)
                    .map(|m| m.name.clone())
                    .collect(),
                snap.date_from.clone(),
                snap.max_size_bytes,
            )
        };
        // Fail-closed. `start` already refused a malformed date, so arriving
        // here with one means the range this session records has become
        // unreadable — and the one answer that is NOT available is carrying
        // on, which would import every message the user excluded and charge
        // their quota for it (§ Quota composition).
        let floor = match ImportDateFloor::parse(&date_from) {
            Ok(f) => f,
            Err(e) => {
                let reason = format!("unreadable since date on this session: {e}");
                return Err(self.fail_and_report(session_id, reason).await);
            }
        };
        if !self.refresh_after_resume().await {
            return Err(self.fail_and_report(session_id, sign_in_again()).await);
        }
        if *self.skip_rest.lock().expect("skip mutex") {
            if let ControlFlow::Break(end) =
                self.skip_rest_walk(&session_id, mailboxes, &floor).await
            {
                return end;
            }
            return self.end_of_walk(session_id).await;
        }
        'mailboxes: for mailbox in mailboxes {
            loop {
                if *self.stop.lock().expect("stop mutex") != StopRequest::None {
                    break 'mailboxes;
                }
                let cursor = self
                    .cursors
                    .lock()
                    .expect("cursors mutex")
                    .get(&mailbox)
                    .copied();

                let status = match self.retrying_source(|| self.source.examine(&mailbox)).await {
                    // § Resume protocol step 4: every stored UID of a rebuilt
                    // mailbox is meaningless, so it is never walked against
                    // the new UID space — the person is offered a restart.
                    Ok(s) if cursor.is_some_and(|c| c.source_uid_validity != s.uid_validity) => {
                        self.note_rebuilt(&mailbox);
                        continue 'mailboxes;
                    }
                    Ok(s) => s,
                    Err(e) => match self.on_source_failure(&session_id, &mailbox, e).await {
                        ControlFlow::Break(end) => return end,
                        ControlFlow::Continue(()) => continue 'mailboxes,
                    },
                };

                let window = match self
                    .retrying_source(|| {
                        self.source.fetch_window(
                            &mailbox,
                            cursor,
                            FETCH_WINDOW_SIZE,
                            max_size_bytes,
                        )
                    })
                    .await
                {
                    Ok(w) => w,
                    Err(e) => match self.on_source_failure(&session_id, &mailbox, e).await {
                        ControlFlow::Break(end) => return end,
                        ControlFlow::Continue(()) => continue 'mailboxes,
                    },
                };

                if window.is_empty() {
                    // Mailbox exhausted.
                    continue 'mailboxes;
                }
                let FetchWindow {
                    outcomes,
                    too_large,
                } = window;
                // § Wizard steps step 3 rules on scope before § Per-message
                // flow step 1 rules on size: a too-large message the since
                // date excludes is out of scope, never "too large". Its date
                // comes from the enumeration; one the source did not date is
                // in scope, the reading that never hides a message silently.
                let (too_large, too_large_out_of_range): (Vec<UidEntry>, Vec<UidEntry>) = too_large
                    .into_iter()
                    .partition(|e| e.internal_date_epoch.is_none_or(|d| floor.admits(d)));

                // StopRequest::Cancel aborts without draining this window —
                // the fetched-but-unsent messages are simply re-fetched (the
                // cursor below has not advanced) if the session is ever
                // resumed. Pause drains it, so the sent-so-far cursor is
                // always durable before the loop actually stops.
                let stop_now = *self.stop.lock().expect("stop mutex");
                if stop_now == StopRequest::Cancel {
                    break 'mailboxes;
                }

                // § Progress lives nest-side's `total_count`: no revision is
                // owed for what the floor turns away below — the Confirm
                // estimate the session opened with already counted only the
                // messages the range admits (`Self::next`).

                let mut packer = BatchPacker::new();
                let mut highest = cursor;
                let mut breached = false;
                // Set when the loop must pause on its own part-way through this
                // window: the first source uid it did not durably hand over, and
                // why. The cursor then stops just short of that uid, so the
                // Resume takes up from it — a message the full mailbox turned
                // away is offered again once there is room, and a unit a dropped
                // connection lost is sent again (the nest's dedup skips whatever
                // of it had landed).
                let mut halt: Option<(u32, ImportPauseReason, String)> = None;
                // What this window decided without sending, for the nest's
                // counters (`record_client_outcomes`).
                let mut unsent: Vec<ClientImportOutcome> = Vec::new();
                for outcome in outcomes {
                    match outcome {
                        FetchOutcome::Fetched(msg) => {
                            highest = Some(MailboxCursor {
                                last_processed_source_uid: msg.uid,
                                source_uid_validity: msg.uid_validity,
                            });
                            // § Wizard steps step 3: a message the since date
                            // excludes is outside the import's SCOPE. It is
                            // never sent, so it never reaches dedup or the
                            // quota check and never moves `skipped_count` —
                            // that counter is for messages the import wanted
                            // and could not take. The cursor above has
                            // already advanced past it, so a Pause here does
                            // not re-walk what the range has ruled out.
                            if !floor.admits(msg.internal_date_epoch) {
                                continue;
                            }
                            for unit in packer.push(*msg) {
                                self.send_unit_or_halt(
                                    &session_id,
                                    &mailbox,
                                    unit,
                                    &mut breached,
                                    &mut halt,
                                )
                                .await?;
                                if breached || halt.is_some() {
                                    break;
                                }
                            }
                        }
                        FetchOutcome::Failed { uid, reason } => {
                            highest = Some(MailboxCursor {
                                last_processed_source_uid: uid,
                                source_uid_validity: status.uid_validity,
                            });
                            breached |=
                                self.record_source_fetch_failure(&mailbox, uid, reason.clone());
                            unsent.push(ClientImportOutcome {
                                uid,
                                kind: ClientImportOutcomeKind::SourceUnreadable { reason },
                                extra: Default::default(),
                            });
                        }
                    }
                    if breached || halt.is_some() {
                        break;
                    }
                }
                if !breached
                    && halt.is_none()
                    && let Some(unit) = packer.flush()
                {
                    self.send_unit_or_halt(&session_id, &mailbox, unit, &mut breached, &mut halt)
                        .await?;
                }
                for entry in too_large.iter().chain(&too_large_out_of_range) {
                    // Whole-window max, not the last element: the two lists
                    // interleave with the fetched UIDs.
                    if highest.is_none_or(|c| entry.uid > c.last_processed_source_uid) {
                        highest = Some(MailboxCursor {
                            last_processed_source_uid: entry.uid,
                            source_uid_validity: status.uid_validity,
                        });
                    }
                }
                // Past a halt, nothing is decided: the Resume walks it again.
                let decided = |uid: u32| halt.as_ref().is_none_or(|(at, ..)| uid < *at);
                for entry in too_large.iter().filter(|e| decided(e.uid)) {
                    let size = entry.size.map_or(0, u64::from);
                    self.push_error_log(format!(
                        "{mailbox} uid {}: too large ({size} bytes, over the {max_size_bytes}-byte limit)",
                        entry.uid
                    ));
                    unsent.push(ClientImportOutcome {
                        uid: entry.uid,
                        kind: ClientImportOutcomeKind::TooLarge { size },
                        extra: Default::default(),
                    });
                }
                unsent.retain(|o| decided(o.uid));
                if !breached && !unsent.is_empty() {
                    let (skipped, errored) =
                        unsent.iter().fold((0u64, 0u64), |(s, e), o| match o.kind {
                            ClientImportOutcomeKind::TooLarge { .. } => (s + 1, e),
                            _ => (s, e + 1),
                        });
                    let first_unsent = unsent.iter().map(|o| o.uid).min().unwrap_or(0);
                    let recorded = self
                        .retrying(|| {
                            self.nest.record_client_outcomes(
                                session_id.clone(),
                                mailbox.clone(),
                                status.uid_validity,
                                unsent.clone(),
                            )
                        })
                        .await;
                    match recorded {
                        Ok(_) => {
                            let mut snap = self.inner.lock().expect("snapshot mutex");
                            snap.skipped_count += skipped;
                            snap.errored_count += errored;
                            if let Some(row) = snap.mailbox_row(&mailbox) {
                                row.skipped += skipped;
                                row.errored += errored;
                            }
                            let mut budget = self.error_budget.lock().expect("error budget mutex");
                            for _ in 0..skipped {
                                breached |= budget.record(false);
                            }
                        }
                        // Unrecorded, so undecided: the Resume walks them again.
                        Err(NestError::Transient(detail)) => {
                            if halt.as_ref().is_none_or(|(at, ..)| first_unsent < *at) {
                                halt =
                                    Some((first_unsent, ImportPauseReason::ConnectionLost, detail));
                            }
                        }
                        Err(e) => return Err(e.into()),
                    }
                }

                if let Some((at, ..)) = &halt {
                    highest = Some(MailboxCursor {
                        last_processed_source_uid: at.saturating_sub(1),
                        source_uid_validity: status.uid_validity,
                    });
                }
                if let Some(c) = highest {
                    self.cursors
                        .lock()
                        .expect("cursors mutex")
                        .insert(mailbox.clone(), c);
                }

                if breached {
                    return Err(self
                        .fail_and_report(session_id, error_budget_exceeded())
                        .await);
                }
                if let Some((_, reason, detail)) = halt {
                    return self.pause_on_its_own(session_id, reason, &detail).await;
                }
                if stop_now == StopRequest::Pause {
                    break 'mailboxes;
                }
            }
        }
        self.end_of_walk(session_id).await
    }

    /// Where both walks end once they stop of their own accord: every
    /// mailbox done (the session finalized), or a Pause or Cancel seen.
    async fn end_of_walk(&self, session_id: String) -> Result<(), DispatchError> {
        match self.end_run() {
            // A rebuilt mailbox is still to be taken: the session pauses on
            // the progress step, where the restart is offered, rather than
            // completing without it.
            StopRequest::None
                if !self
                    .inner
                    .lock()
                    .expect("snapshot mutex")
                    .rebuilt_mailboxes
                    .is_empty() =>
            {
                return self
                    .pause_on_its_own(
                        session_id,
                        ImportPauseReason::MailboxRebuilt,
                        "a source mailbox's UIDVALIDITY changed",
                    )
                    .await;
            }
            StopRequest::None => {
                let session = self.nest.finalize_session(session_id).await?;
                self.store_session(session);
                let _ = self.source.logout().await;
                self.forget_microsoft_token();
            }
            // The Pause pressed while this run was in flight (`pause`): its
            // window is drained, so the session can stop now. One the nest
            // already holds paused — pressed before the run began — is left.
            StopRequest::Pause => {
                let running = self.inner.lock().expect("snapshot mutex").session_state
                    == Some(ImportSessionState::Running);
                if running {
                    let session = self.nest.pause_session(session_id).await?;
                    self.store_session(session);
                }
            }
            StopRequest::Cancel => {}
        }
        Ok(())
    }

    /// A source call that failed through its retries (§ Failure handling →
    /// *Source-side errors*): a refused sign-in ends the session asking for
    /// new credentials, a dropped connection pauses it with the cursor kept
    /// (both `Break`, with the run's answer), and any other refusal is that
    /// mailbox's alone — logged, and the walk goes on to the next
    /// (`Continue`).
    async fn on_source_failure(
        &self,
        session_id: &str,
        mailbox: &str,
        e: NestError,
    ) -> ControlFlow<Result<(), DispatchError>> {
        let session_id = session_id.to_string();
        match e {
            NestError::Rejected(_) if self.take_sign_in_again() => {
                ControlFlow::Break(Err(self.fail_and_report(session_id, sign_in_again()).await))
            }
            NestError::Rejected(_) if self.source.take_sign_in_refused() => {
                let reason = self.sign_in_refused_sentence();
                ControlFlow::Break(Err(self.fail_and_report(session_id, reason).await))
            }
            NestError::Rejected(reason) => {
                self.push_error_log(format!("{mailbox}: {reason}"));
                ControlFlow::Continue(())
            }
            NestError::Transient(reason) => ControlFlow::Break(
                self.pause_on_its_own(session_id, ImportPauseReason::ConnectionLost, &reason)
                    .await,
            ),
        }
    }

    /// "Skip the rest" ([`MailImportAction::SkipRest`]): walk what is left of
    /// the scope by UID alone, never fetching a body, and record each message
    /// the import still wanted as a plain skip — the message the full mailbox
    /// turned away first, which lifts the nest's quota hold (§ Progress lives
    /// nest-side → *The quota hold*). A message the since date excludes is out
    /// of scope and is passed over uncounted, as the import walk does.
    ///
    /// The counters come from the nest's answer, not a local tally: the hold
    /// makes the walk re-offer messages the nest already counted (the
    /// turned-away one, and any of its batch that landed), which the nest
    /// counts once. `Continue` when the walk stopped of its own accord —
    /// every mailbox done, or a Pause or Cancel seen between windows.
    async fn skip_rest_walk(
        &self,
        session_id: &str,
        mailboxes: Vec<String>,
        floor: &ImportDateFloor,
    ) -> ControlFlow<Result<(), DispatchError>> {
        'mailboxes: for mailbox in mailboxes {
            loop {
                if *self.stop.lock().expect("stop mutex") != StopRequest::None {
                    break 'mailboxes;
                }
                let cursor = self
                    .cursors
                    .lock()
                    .expect("cursors mutex")
                    .get(&mailbox)
                    .copied();
                let status = match self.retrying_source(|| self.source.examine(&mailbox)).await {
                    // A rebuilt mailbox is the restart offer's, as in the
                    // import walk (§ Resume protocol step 4).
                    Ok(s) if cursor.is_some_and(|c| c.source_uid_validity != s.uid_validity) => {
                        self.note_rebuilt(&mailbox);
                        continue 'mailboxes;
                    }
                    Ok(s) => s,
                    Err(e) => match self.on_source_failure(session_id, &mailbox, e).await {
                        ControlFlow::Break(end) => return ControlFlow::Break(end),
                        ControlFlow::Continue(()) => continue 'mailboxes,
                    },
                };
                let entries = match self
                    .retrying_source(|| {
                        self.source
                            .enumerate_window(&mailbox, cursor, FETCH_WINDOW_SIZE)
                    })
                    .await
                {
                    Ok(entries) => entries,
                    Err(e) => match self.on_source_failure(session_id, &mailbox, e).await {
                        ControlFlow::Break(end) => return ControlFlow::Break(end),
                        ControlFlow::Continue(()) => continue 'mailboxes,
                    },
                };
                let Some(last) = entries.iter().map(|e| e.uid).max() else {
                    continue 'mailboxes;
                };
                let skipped: Vec<ClientImportOutcome> = entries
                    .iter()
                    .filter(|e| e.internal_date_epoch.is_none_or(|d| floor.admits(d)))
                    .map(|e| ClientImportOutcome {
                        uid: e.uid,
                        kind: ClientImportOutcomeKind::SkippedByUser,
                        extra: Default::default(),
                    })
                    .collect();
                if !skipped.is_empty() {
                    let recorded = self
                        .retrying(|| {
                            self.nest.record_client_outcomes(
                                session_id.to_string(),
                                mailbox.clone(),
                                status.uid_validity,
                                skipped.clone(),
                            )
                        })
                        .await;
                    match recorded {
                        Ok(counters) => {
                            let mut snap = self.inner.lock().expect("snapshot mutex");
                            let newly = counters.skipped_count.saturating_sub(snap.skipped_count);
                            snap.imported_count = counters.imported_count;
                            snap.skipped_count = counters.skipped_count;
                            snap.errored_count = counters.errored_count;
                            if let Some(row) = snap.mailbox_row(&mailbox) {
                                row.skipped += newly;
                            }
                        }
                        // Unrecorded, so the cursor stays: the Resume walks
                        // this window again.
                        Err(NestError::Transient(detail)) => {
                            return ControlFlow::Break(
                                self.pause_on_its_own(
                                    session_id.to_string(),
                                    ImportPauseReason::ConnectionLost,
                                    &detail,
                                )
                                .await,
                            );
                        }
                        Err(e) => return ControlFlow::Break(Err(e.into())),
                    }
                }
                self.cursors.lock().expect("cursors mutex").insert(
                    mailbox.clone(),
                    MailboxCursor {
                        last_processed_source_uid: last,
                        source_uid_validity: status.uid_validity,
                    },
                );
            }
        }
        ControlFlow::Continue(())
    }

    /// The run's last look at the stop request, taken as it stops counting
    /// as in flight — one step under the `stop` lock, so a Pause pressed at
    /// this moment is either seen here or pauses the nest itself.
    fn end_run(&self) -> StopRequest {
        let stop = self.stop.lock().expect("stop mutex");
        *self.run_active.lock().expect("run mutex") = false;
        *stop
    }

    /// Client-side wizard mutations (no seam round-trip). Returns `true` when
    /// the action was handled here, so `dispatch` skips the seam path.
    fn apply_client_action(&self, action: &MailImportAction) -> bool {
        let mut snap = self.inner.lock().expect("snapshot mutex");
        match action {
            MailImportAction::SelectSourceKind { kind } => {
                snap.source_kind = *kind;
                if let Some((host, port)) = provider_preset(*kind) {
                    snap.host = host.to_string();
                    snap.port = port;
                    snap.tls_mode = ImportTlsMode::Implicit;
                }
            }
            MailImportAction::SetHost { value } => snap.host = value.clone(),
            MailImportAction::SetPort { value } => snap.port = *value,
            MailImportAction::SetTlsMode { mode } => snap.tls_mode = *mode,
            MailImportAction::SetUsername { value } => snap.username = value.clone(),
            MailImportAction::SetPassword { value } => snap.password = value.clone(),
            MailImportAction::ToggleMailbox { mailbox } => {
                if let Some(m) = snap.mailboxes.iter_mut().find(|m| &m.name == mailbox) {
                    m.selected = !m.selected;
                }
            }
            MailImportAction::SetDateFrom { value } => snap.date_from = value.clone(),
            MailImportAction::SetMaxSizeBytes { value } => snap.max_size_bytes = *value,
            MailImportAction::SetImportDuplicates { value } => snap.import_duplicates = *value,
            // Scope → Confirm asks the source first (`Self::next`).
            MailImportAction::Next if snap.step != ImportStep::Scope => {}
            MailImportAction::Back => snap.step = prev_step(snap.step),
            _ => return false,
        }
        true
    }
}

/// Back: Confirm → Scope → Source. Forward is `Next` (Scope → Confirm, after
/// the estimate) and `Connect` (Source → Scope).
fn prev_step(step: ImportStep) -> ImportStep {
    match step {
        ImportStep::Confirm => ImportStep::Scope,
        ImportStep::Scope => ImportStep::Source,
        other => other,
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl MailImportMachine {
    pub fn snapshot(&self) -> MailImportSnapshot {
        fauna_core::clone_locked(&self.inner, |s| s)
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl MailImportMachine {
    /// Initial page load.
    pub async fn hydrate(&self) -> Result<(), DispatchError> {
        self.refresh().await
    }

    /// The fetch-drive loop — see the module docs for the full design. Call
    /// once per `Start`/`Resume` dispatch (app glue spawns this once per
    /// platform, native `tokio::spawn` / wasm `spawn_local`, right after a
    /// successful `Start`/`Resume`); returns when every selected mailbox is
    /// exhausted (session `finalize`d), or when `Pause`/`Cancel` was
    /// dispatched concurrently, or on a session-fatal error.
    pub async fn run_import(&self) -> Result<(), DispatchError> {
        // An adopted session whose Resume has not reached the source yet
        // (module docs, *Resume*) holds no source connection: walking it would
        // answer "not connected" for every mailbox and finalize a session that
        // read nothing. An app spawns a run whenever the row reads `running`,
        // which a row a closed app left behind already does — even after a
        // refused Resume — so the refusal lives here, for all of them. Nothing
        // is touched: not the row, and not the snapshot's error, which still
        // says why that Resume was refused.
        let (cold, kind) = {
            let snap = self.inner.lock().expect("snapshot mutex");
            (snap.resume_needs_credentials, snap.source_kind)
        };
        if cold {
            use fauna_i18n::strings::mail_import as s;
            let sentence = match kind {
                ImportSourceKind::Outlook => s::RESUME_NEEDS_MICROSOFT,
                _ => s::RESUME_NEEDS_PASSWORD,
            };
            return Err(DispatchError::InvalidState(sentence.into()));
        }
        {
            let _stop = self.stop.lock().expect("stop mutex");
            *self.run_active.lock().expect("run mutex") = true;
        }
        let result = self.run_import_inner().await;
        self.end_run();
        if let Err(ref e) = result {
            self.forget_microsoft_token();
            let mut snap = self.inner.lock().expect("snapshot mutex");
            crate::state::set_snapshot_error(&mut snap.error, e.to_string());
            snap.status = ImportStatus::Idle;
        }
        result
    }

    /// The Microsoft sign-in's polling half — call once after a successful
    /// `BeginMicrosoftSignIn` dispatch (app glue spawns it exactly as it
    /// spawns `run_import`). Returns when the person finished (and the
    /// source connected, or failed to), declined, let the code expire, or
    /// the sign-in was cancelled; the snapshot carries the outcome.
    pub async fn run_microsoft_sign_in(&self) -> Result<(), DispatchError> {
        let result = self.run_microsoft_sign_in_inner().await;
        if let Err(ref e) = result {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            crate::state::set_snapshot_error(&mut snap.error, e.to_string());
            snap.status = ImportStatus::Idle;
        }
        result
    }

    pub async fn dispatch(&self, action: MailImportAction) -> Result<(), DispatchError> {
        self.inner.lock().expect("snapshot mutex").error = None;
        if self.apply_client_action(&action) {
            return Ok(());
        }
        crate::dispatch_capturing_error!(
            self,
            ImportStatus,
            match action {
                MailImportAction::Refresh => self.refresh().await,
                MailImportAction::Connect => self.connect().await,
                MailImportAction::ForgetSource => self.forget_source().await,
                MailImportAction::BeginMicrosoftSignIn => self.begin_microsoft_sign_in().await,
                MailImportAction::CancelMicrosoftSignIn => {
                    self.cancel_microsoft_sign_in();
                    Ok(())
                }
                MailImportAction::Next => self.next().await,
                MailImportAction::Start => self.start().await,
                MailImportAction::Pause => self.pause().await,
                MailImportAction::Resume => self.resume().await,
                MailImportAction::Cancel => self.cancel().await,
                MailImportAction::RestartRebuiltMailboxes => {
                    self.restart_rebuilt_mailboxes().await
                }
                MailImportAction::SkipRest => self.skip_the_rest().await,
                MailImportAction::Dismiss => self.dismiss().await,
                // The client-side variants were handled above.
                _ => Ok(()),
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_mail::imap_client::FetchedMessage;
    use std::collections::HashSet;
    use std::sync::Mutex as StdMutex;

    /// In-memory nest modelling a working import backend.
    struct FakeNest {
        session: StdMutex<Option<ImportSessionView>>,
        /// Every source_uid ever sent via `send_unit`, in call order — what
        /// `run_import` tests assert against to prove exactly what was (and
        /// was not) sent.
        sent_uids: StdMutex<Vec<u32>>,
        /// `source_uid`s that should come back `Errored` from `send_unit`
        /// instead of `Imported`.
        error_uids: StdMutex<HashSet<u32>>,
        /// Every `record_client_outcomes` call: `(mailbox, outcomes)`.
        recorded: StdMutex<Vec<(String, Vec<ClientImportOutcome>)>>,
        /// `source_uid`s the mailbox has no room for — answered `Skipped`,
        /// reason `quota_exceeded`, exactly as the real handler does.
        quota_uids: StdMutex<HashSet<u32>>,
        /// `source_uid`s the nest already holds — answered `Skipped`, reason
        /// `dedup`, exactly as the real handler does.
        dedup_uids: StdMutex<HashSet<u32>>,
        /// Errors `send_unit` returns once each, in order, before answering.
        send_errors: StdMutex<VecDeque<NestError>>,
        /// Errors `start_session` returns once each, in order, before opening.
        start_errors: StdMutex<VecDeque<NestError>>,
        /// Every request the machine sent, serialized — § Microsoft sign-in's
        /// tier_1 witness reads these for the token's bytes.
        outbound: StdMutex<Vec<String>>,
        /// What `microsoft_registration` answers — the admin's pair off
        /// `fauna.setup.status`, `None` by default.
        registration: StdMutex<Option<MicrosoftRegistration>>,
        /// A whole listing, newest first, in place of the one `session` —
        /// the adoption rule's tests list several rows.
        listing: StdMutex<Option<Vec<ImportSessionView>>>,
        /// `(entered, release)`: `list_sessions` reads its answer, signals
        /// `entered`, and waits for `release` before returning it — a stale
        /// listing in flight while the test moves the machine's view.
        list_gate: StdMutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
    }

    impl FakeNest {
        fn new() -> Self {
            Self {
                session: StdMutex::new(None),
                sent_uids: StdMutex::new(Vec::new()),
                error_uids: StdMutex::new(HashSet::new()),
                recorded: StdMutex::new(Vec::new()),
                quota_uids: StdMutex::new(HashSet::new()),
                dedup_uids: StdMutex::new(HashSet::new()),
                send_errors: StdMutex::new(VecDeque::new()),
                start_errors: StdMutex::new(VecDeque::new()),
                outbound: StdMutex::new(Vec::new()),
                registration: StdMutex::new(None),
                listing: StdMutex::new(None),
                list_gate: StdMutex::new(None),
            }
        }

        /// The calls the machine made, by name, in order.
        fn calls(&self) -> Vec<String> {
            self.outbound
                .lock()
                .unwrap()
                .iter()
                .map(|c| c.split(' ').next().unwrap_or_default().to_string())
                .collect()
        }

        fn record(&self, call: &str, args: impl Serialize) {
            let args = serde_json::to_string(&args).expect("serializable request");
            self.outbound.lock().unwrap().push(format!("{call} {args}"));
        }

        fn with_quota_uids(self, uids: impl IntoIterator<Item = u32>) -> Self {
            *self.quota_uids.lock().unwrap() = uids.into_iter().collect();
            self
        }

        fn with_dedup_uids(self, uids: impl IntoIterator<Item = u32>) -> Self {
            *self.dedup_uids.lock().unwrap() = uids.into_iter().collect();
            self
        }

        fn queue_send_error(self, e: NestError) -> Self {
            self.send_errors.lock().unwrap().push_back(e);
            self
        }

        fn queue_start_error(self, e: NestError) -> Self {
            self.start_errors.lock().unwrap().push_back(e);
            self
        }

        fn with_error_uids(self, uids: impl IntoIterator<Item = u32>) -> Self {
            *self.error_uids.lock().unwrap() = uids.into_iter().collect();
            self
        }
    }

    fn session(state: ImportSessionState, total: u64) -> ImportSessionView {
        ImportSessionView {
            session_id: "sess-1".into(),
            state,
            source_descriptor: "gmail:user@example.com".into(),
            total_count: total,
            imported_count: 0,
            skipped_count: 0,
            errored_count: 0,
            error_reason: String::new(),
            scope: Vec::new(),
            date_from: String::new(),
            skip_dedup: false,
            cursors: Vec::new(),
            started_at: 1_000,
            last_progress_at: 1_000,
        }
    }

    /// The Done step's "time elapsed" and the progress step's estimate read
    /// the session row's own clock (§ Wizard steps steps 5–6).
    #[test]
    fn the_snapshot_times_the_run_from_the_session_row() {
        let mut snap = MailImportSnapshot::empty();
        snap.apply_session(ImportSessionView {
            imported_count: 8,
            skipped_count: 2,
            last_progress_at: 1_040,
            ..session(ImportSessionState::Running, 40)
        });
        let t = snap.timing(1_060);
        assert_eq!(t.elapsed_secs, Some(60));
        assert_eq!(t.remaining_secs, Some(180), "skips count as processed");
        snap.apply_session(ImportSessionView {
            imported_count: 40,
            last_progress_at: 1_150,
            ..session(ImportSessionState::Completed, 40)
        });
        let t = snap.timing(9_999);
        assert_eq!(t.elapsed_secs, Some(150), "frozen at completion");
        assert_eq!(t.remaining_secs, None);
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl MailImportNest for FakeNest {
        async fn list_sessions(&self) -> Result<Vec<ImportSessionView>, NestError> {
            self.record("list_sessions", ());
            let answer = match self.listing.lock().unwrap().clone() {
                Some(listing) => listing,
                None => self.session.lock().unwrap().clone().into_iter().collect(),
            };
            let gate = self.list_gate.lock().unwrap().take();
            if let Some((entered, release)) = gate {
                entered.notify_one();
                release.notified().await;
            }
            Ok(answer)
        }
        async fn microsoft_registration(&self) -> Result<Option<MicrosoftRegistration>, NestError> {
            Ok(self.registration.lock().unwrap().clone())
        }
        async fn start_session(
            &self,
            _source_descriptor: String,
            total_count: u64,
            scope: Vec<String>,
            date_from: String,
            skip_dedup: bool,
        ) -> Result<ImportSessionView, NestError> {
            self.record(
                "start_session",
                (&_source_descriptor, total_count, &scope, &date_from),
            );
            if let Some(e) = self.start_errors.lock().unwrap().pop_front() {
                return Err(e);
            }
            let mut s = session(ImportSessionState::Running, total_count);
            s.scope = scope;
            s.date_from = date_from;
            s.skip_dedup = skip_dedup;
            *self.session.lock().unwrap() = Some(s.clone());
            Ok(s)
        }
        async fn pause_session(&self, _session_id: String) -> Result<ImportSessionView, NestError> {
            self.record("pause_session", &_session_id);
            let mut guard = self.session.lock().unwrap();
            let mut s = guard
                .clone()
                .ok_or(NestError::Rejected("no session".into()))?;
            s.state = ImportSessionState::Paused;
            *guard = Some(s.clone());
            Ok(s)
        }
        async fn resume_session(
            &self,
            _session_id: String,
        ) -> Result<ImportSessionView, NestError> {
            self.record("resume_session", &_session_id);
            let mut guard = self.session.lock().unwrap();
            let mut s = guard
                .clone()
                .ok_or(NestError::Rejected("no session".into()))?;
            s.state = ImportSessionState::Running;
            *guard = Some(s.clone());
            Ok(s)
        }
        async fn cancel_session(
            &self,
            _session_id: String,
        ) -> Result<ImportSessionView, NestError> {
            self.record("cancel_session", &_session_id);
            let mut guard = self.session.lock().unwrap();
            let mut s = guard
                .clone()
                .ok_or(NestError::Rejected("no session".into()))?;
            s.state = ImportSessionState::Cancelled;
            *guard = Some(s.clone());
            Ok(s)
        }
        async fn discard_session(&self, session_id: String) -> Result<bool, NestError> {
            self.record("discard_session", &session_id);
            // The real handler's rule: a terminal row goes, a live one stays.
            let mut guard = self.session.lock().unwrap();
            match guard.as_ref() {
                Some(s) if s.session_id == session_id => {
                    if matches!(
                        s.state,
                        ImportSessionState::Running | ImportSessionState::Paused
                    ) {
                        return Err(NestError::Rejected(format!(
                            "import session '{session_id}' is live"
                        )));
                    }
                    *guard = None;
                    Ok(true)
                }
                _ => Ok(false),
            }
        }
        async fn finalize_session(
            &self,
            _session_id: String,
        ) -> Result<ImportSessionView, NestError> {
            self.record("finalize_session", &_session_id);
            let mut guard = self.session.lock().unwrap();
            let mut s = guard
                .clone()
                .ok_or(NestError::Rejected("no session".into()))?;
            s.state = ImportSessionState::Completed;
            *guard = Some(s.clone());
            Ok(s)
        }
        async fn fail_session(
            &self,
            _session_id: String,
            reason: String,
        ) -> Result<ImportSessionView, NestError> {
            self.record("fail_session", (&_session_id, &reason));
            let mut guard = self.session.lock().unwrap();
            let mut s = guard
                .clone()
                .ok_or(NestError::Rejected("no session".into()))?;
            s.state = ImportSessionState::Errored;
            s.error_reason = reason;
            *guard = Some(s.clone());
            Ok(s)
        }
        async fn send_unit(
            &self,
            _session_id: String,
            unit: ImportUnit,
            skip_dedup: bool,
            revised_total_count: Option<u64>,
        ) -> Result<SendUnitReply, NestError> {
            let wire: Vec<&ImportMessageItem> = match &unit {
                ImportUnit::Batch(items) => items.iter().collect(),
                ImportUnit::Single(item) => vec![item.as_ref()],
            };
            self.record("send_unit", (&_session_id, &wire, revised_total_count));
            if let Some(e) = self.send_errors.lock().unwrap().pop_front() {
                return Err(e);
            }
            // The real handler takes messages only into a running session
            // (`bridge_import_handlers.rs`: "is paused, not running").
            if let Some(s) = self.session.lock().unwrap().as_ref()
                && s.state != ImportSessionState::Running
            {
                return Err(NestError::Rejected(format!(
                    "import session '{}' is {:?}, not running",
                    s.session_id, s.state
                )));
            }
            let items: Vec<ImportMessageItem> = match unit {
                ImportUnit::Batch(items) => items,
                ImportUnit::Single(item) => vec![*item],
            };
            let quota_uids = self.quota_uids.lock().unwrap();
            let error_uids = self.error_uids.lock().unwrap();
            let dedup_uids = self.dedup_uids.lock().unwrap();
            let mut sent = self.sent_uids.lock().unwrap();
            let mut session = self.session.lock().unwrap();
            // `total_count = COALESCE(?, total_count)`, exactly as the real
            // handler applies it — a revision replaces the estimate, and a
            // smaller one is the whole point under a date floor.
            if let (Some(revised), Some(s)) = (revised_total_count, session.as_mut()) {
                s.total_count = revised;
            }
            let outcomes = items
                .iter()
                .map(|item| {
                    sent.push(item.source_uid);
                    if quota_uids.contains(&item.source_uid) {
                        if let Some(s) = session.as_mut() {
                            s.skipped_count += 1;
                        }
                        SendOutcome::Skipped {
                            reason: IMPORT_SKIP_QUOTA_EXCEEDED.into(),
                        }
                    } else if !skip_dedup && dedup_uids.contains(&item.source_uid) {
                        // The real handler's `skip_dedup` imports again
                        // instead of skipping (§ Opt-out per session).
                        if let Some(s) = session.as_mut() {
                            s.skipped_count += 1;
                        }
                        SendOutcome::Skipped {
                            reason: "dedup".into(),
                        }
                    } else if error_uids.contains(&item.source_uid) {
                        if let Some(s) = session.as_mut() {
                            s.errored_count += 1;
                        }
                        SendOutcome::Errored {
                            reason: "synthetic test failure".into(),
                        }
                    } else {
                        if let Some(s) = session.as_mut() {
                            s.imported_count += 1;
                        }
                        SendOutcome::Imported
                    }
                })
                .collect();
            Ok(SendUnitReply { outcomes })
        }
        async fn record_client_outcomes(
            &self,
            _session_id: String,
            mailbox: String,
            _uid_validity: u32,
            outcomes: Vec<ClientImportOutcome>,
        ) -> Result<RecordImportOutcomesReply, NestError> {
            self.record(
                "record_client_outcomes",
                (&_session_id, &mailbox, _uid_validity, &outcomes),
            );
            // The real handler's fold: too large and skipped by the user are
            // skips, anything else a failure — and, as the quota hold's
            // count-once rule has it, a message already sent was already
            // counted.
            let sent = self.sent_uids.lock().unwrap().clone();
            let mut reply = RecordImportOutcomesReply::default();
            if let Some(s) = self.session.lock().unwrap().as_mut() {
                for o in &outcomes {
                    match o.kind {
                        ClientImportOutcomeKind::SkippedByUser if sent.contains(&o.uid) => {}
                        ClientImportOutcomeKind::TooLarge { .. }
                        | ClientImportOutcomeKind::SkippedByUser => s.skipped_count += 1,
                        _ => s.errored_count += 1,
                    }
                }
                reply.imported_count = s.imported_count;
                reply.skipped_count = s.skipped_count;
                reply.errored_count = s.errored_count;
            }
            self.recorded.lock().unwrap().push((mailbox, outcomes));
            Ok(reply)
        }
    }

    /// Where one `connect` dialled: `(kind, host, port, tls, username)`.
    type Dialled = (ImportSourceKind, String, u16, ImportTlsMode, String);

    /// In-memory foreign IMAP source.
    struct FakeSource {
        mailboxes: Vec<SourceMailboxView>,
        /// The count `estimate_mailbox` answers for a mailbox with no
        /// `messages` (the constructor's third column), unsized.
        counts: HashMap<String, u32>,
        /// Every `estimate_mailbox` call: `(mailbox, since_epoch)`.
        estimated: StdMutex<Vec<(String, Option<i64>)>>,
        /// Per-mailbox: an error `estimate_mailbox` answers with, once.
        estimate_errors: StdMutex<HashMap<String, NestError>>,
        /// Set to force `connect` to fail (bad credentials).
        reject: bool,
        /// The failure a rejecting `connect` answers with.
        connect_failure: SourceConnectError,
        uid_validity: u32,
        /// mailbox name -> its fetchable messages, ascending uid.
        messages: HashMap<String, Vec<FetchedMessage>>,
        /// uids that come back as a per-message `FetchOutcome::Failed`
        /// instead of `Fetched` (a source-side FETCH failure).
        fail_uids: HashSet<u32>,
        /// Per-mailbox: errors `examine` returns once each, in order, before
        /// falling through to normal behaviour.
        examine_errors: StdMutex<HashMap<String, VecDeque<NestError>>>,
        /// Same, for `fetch_window`.
        fetch_errors: StdMutex<HashMap<String, VecDeque<NestError>>>,
        /// Every `fetch_window` call's mailbox — the calls that move bodies.
        fetched: StdMutex<Vec<String>>,
        /// Every `enumerate_window` call's mailbox.
        enumerated: StdMutex<Vec<String>>,
        logged_out: StdMutex<bool>,
        /// `can_ever_connect`'s answer — models the web stub.
        can_never_connect: bool,
        /// Where every `connect` dialled: `(kind, host, port, tls, username)`.
        connects: StdMutex<Vec<Dialled>>,
        /// Every credential `connect` and `replace_auth` were handed.
        auths: StdMutex<Vec<SourceAuth>>,
        /// Mailboxes whose next `fetch_window` signs in again and has the
        /// kept credential refused, once each.
        refuse_sign_in_at_fetch: StdMutex<HashSet<String>>,
        /// What `take_sign_in_refused` answers next.
        sign_in_refused: StdMutex<bool>,
        /// A mailbox whose `fetch_window` signals `reached` and then waits
        /// for `release` — a run parked mid-window, so a test can act while
        /// the window is in flight.
        fetch_gate: Option<(String, Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    }

    impl FakeSource {
        fn new(mailboxes: &[(&str, bool, u32)]) -> Self {
            Self {
                mailboxes: mailboxes
                    .iter()
                    .map(|(name, selectable, _)| SourceMailboxView {
                        name: name.to_string(),
                        selectable: *selectable,
                        special_use: None,
                    })
                    .collect(),
                counts: mailboxes
                    .iter()
                    .map(|(name, _, count)| (name.to_string(), *count))
                    .collect(),
                estimated: StdMutex::new(Vec::new()),
                estimate_errors: StdMutex::new(HashMap::new()),
                reject: false,
                connect_failure: SourceConnectError::new(
                    SourceConnectFailure::SignIn,
                    "bad credentials",
                ),
                uid_validity: 7,
                messages: HashMap::new(),
                fail_uids: HashSet::new(),
                examine_errors: StdMutex::new(HashMap::new()),
                fetch_errors: StdMutex::new(HashMap::new()),
                fetched: StdMutex::new(Vec::new()),
                enumerated: StdMutex::new(Vec::new()),
                logged_out: StdMutex::new(false),
                can_never_connect: false,
                connects: StdMutex::new(Vec::new()),
                auths: StdMutex::new(Vec::new()),
                refuse_sign_in_at_fetch: StdMutex::new(HashSet::new()),
                sign_in_refused: StdMutex::new(false),
                fetch_gate: None,
            }
        }

        /// Park `mailbox`'s first `fetch_window` (see `fetch_gate`); returns
        /// the `(reached, release)` pair the test drives it by.
        fn gating_fetch(
            mut self,
            mailbox: &str,
        ) -> (Self, Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
            let reached = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            self.fetch_gate = Some((mailbox.to_string(), reached.clone(), release.clone()));
            (self, reached, release)
        }

        fn rejecting() -> Self {
            Self {
                mailboxes: Vec::new(),
                reject: true,
                ..Self::new(&[])
            }
        }

        /// `connect` fails with `failure`, the source saying `detail`.
        fn failing_connect(failure: SourceConnectFailure, detail: &str) -> Self {
            Self {
                connect_failure: SourceConnectError::new(failure, detail),
                ..Self::rejecting()
            }
        }

        /// Models a seam whose transport does not exist (web, today) —
        /// `connect` always fails and `can_ever_connect` says so.
        fn never_connectable() -> Self {
            Self {
                can_never_connect: true,
                ..Self::rejecting()
            }
        }

        /// Populate `mailbox` with `count` fetchable messages, uids `1..=count`.
        fn with_messages(mut self, mailbox: &str, count: u32) -> Self {
            let uid_validity = self.uid_validity;
            let msgs = (1..=count)
                .map(|uid| FetchedMessage {
                    mailbox: mailbox.to_string(),
                    uid,
                    uid_validity,
                    flags: Vec::new(),
                    internal_date_epoch: 0,
                    body: format!("Subject: msg {uid}\r\n\r\nbody {uid}").into_bytes(),
                })
                .collect();
            self.messages.insert(mailbox.to_string(), msgs);
            self
        }

        /// Populate `mailbox` with messages at explicit INTERNALDATEs —
        /// `(uid, internal_date_epoch)` pairs, which is what a date-filtered
        /// walk is decided on (`mailbox-migration.md` § Wizard steps step 3).
        fn with_dated_messages(mut self, mailbox: &str, dated: &[(u32, i64)]) -> Self {
            let uid_validity = self.uid_validity;
            let msgs = dated
                .iter()
                .map(|(uid, epoch)| FetchedMessage {
                    mailbox: mailbox.to_string(),
                    uid: *uid,
                    uid_validity,
                    flags: Vec::new(),
                    internal_date_epoch: *epoch,
                    body: format!("Subject: msg {uid}\r\n\r\nbody {uid}").into_bytes(),
                })
                .collect();
            self.messages.insert(mailbox.to_string(), msgs);
            self
        }

        /// Advertise `role` on `mailbox`'s `LIST` line.
        fn with_special_use(mut self, mailbox: &str, role: SpecialUse) -> Self {
            self.mailboxes
                .iter_mut()
                .find(|m| m.name == mailbox)
                .expect("mailbox exists")
                .special_use = Some(role);
            self
        }

        fn failing_uid(mut self, uid: u32) -> Self {
            self.fail_uids.insert(uid);
            self
        }

        /// Make `uid` of `mailbox` a `size`-byte message dated `epoch`.
        fn sized(mut self, mailbox: &str, uid: u32, size: usize, epoch: i64) -> Self {
            let m = self
                .messages
                .get_mut(mailbox)
                .and_then(|ms| ms.iter_mut().find(|m| m.uid == uid))
                .expect("message exists");
            m.body = vec![b'x'; size];
            m.internal_date_epoch = epoch;
            self
        }

        fn failing_estimate(self, mailbox: &str, e: NestError) -> Self {
            self.estimate_errors
                .lock()
                .unwrap()
                .insert(mailbox.to_string(), e);
            self
        }

        fn queue_examine_error(self, mailbox: &str, e: NestError) -> Self {
            self.examine_errors
                .lock()
                .unwrap()
                .entry(mailbox.to_string())
                .or_default()
                .push_back(e);
            self
        }

        /// The source stops taking the password: `mailbox`'s next fetch
        /// window signs in again, as the real seam does after a dropped
        /// connection, and is refused.
        fn refusing_sign_in_at_fetch(self, mailbox: &str) -> Self {
            self.refuse_sign_in_at_fetch
                .lock()
                .unwrap()
                .insert(mailbox.to_string());
            self
        }

        fn queue_fetch_error(self, mailbox: &str, e: NestError) -> Self {
            self.fetch_errors
                .lock()
                .unwrap()
                .entry(mailbox.to_string())
                .or_default()
                .push_back(e);
            self
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl ImportSourceNest for FakeSource {
        async fn connect(&self, params: SourceConnectParams) -> Result<(), SourceConnectError> {
            self.connects.lock().unwrap().push((
                params.kind,
                params.host.clone(),
                params.port,
                params.tls_mode,
                params.username.clone(),
            ));
            self.auths.lock().unwrap().push(params.auth.clone());
            if self.reject {
                return Err(self.connect_failure.clone());
            }
            assert!(!params.username.is_empty(), "connect needs a username");
            Ok(())
        }
        async fn list_source_mailboxes(&self) -> Result<Vec<SourceMailboxView>, NestError> {
            Ok(self.mailboxes.clone())
        }
        /// The real seam's enumeration path, over the body's length as its
        /// `RFC822.SIZE`; a mailbox with no messages answers its constructor
        /// count, unsized.
        async fn estimate_mailbox(
            &self,
            mailbox: &str,
            since_epoch: Option<i64>,
        ) -> Result<MailboxEstimate, NestError> {
            self.estimated
                .lock()
                .unwrap()
                .push((mailbox.to_string(), since_epoch));
            if let Some(e) = self.estimate_errors.lock().unwrap().remove(mailbox) {
                return Err(e);
            }
            let Some(messages) = self.messages.get(mailbox) else {
                return Ok(MailboxEstimate {
                    messages: self.counts.get(mailbox).copied().unwrap_or(0),
                    bytes: None,
                });
            };
            let in_range: Vec<&FetchedMessage> = messages
                .iter()
                .filter(|m| since_epoch.is_none_or(|s| m.internal_date_epoch >= s))
                .collect();
            Ok(MailboxEstimate {
                messages: in_range.len() as u32,
                bytes: Some(in_range.iter().map(|m| m.body.len() as u64).sum()),
            })
        }
        async fn examine(&self, mailbox: &str) -> Result<MailboxStatus, NestError> {
            if let Some(e) = self
                .examine_errors
                .lock()
                .unwrap()
                .get_mut(mailbox)
                .and_then(VecDeque::pop_front)
            {
                return Err(e);
            }
            let exists = self.messages.get(mailbox).map_or(0, |m| m.len() as u32);
            Ok(MailboxStatus {
                uid_validity: self.uid_validity,
                exists,
            })
        }
        async fn fetch_window(
            &self,
            mailbox: &str,
            cursor: Option<MailboxCursor>,
            max: usize,
            max_size_bytes: u64,
        ) -> Result<FetchWindow, NestError> {
            self.fetched.lock().unwrap().push(mailbox.to_string());
            if self.refuse_sign_in_at_fetch.lock().unwrap().remove(mailbox) {
                *self.sign_in_refused.lock().unwrap() = true;
                return Err(NestError::Rejected(
                    "NO [AUTHENTICATIONFAILED] Invalid credentials".into(),
                ));
            }
            if let Some(e) = self
                .fetch_errors
                .lock()
                .unwrap()
                .get_mut(mailbox)
                .and_then(VecDeque::pop_front)
            {
                return Err(e);
            }
            if let Some((gated, reached, release)) = &self.fetch_gate
                && gated == mailbox
                && cursor.is_none()
            {
                reached.notify_one();
                release.notified().await;
            }
            let from = cursor.map_or(1, |c| c.last_processed_source_uid + 1);
            // The body's length stands in for `RFC822.SIZE`, as the real
            // seam's enumeration reports it, dated like the real one.
            let (too_large, wanted): (Vec<FetchedMessage>, Vec<FetchedMessage>) = self
                .messages
                .get(mailbox)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|m| m.uid >= from)
                .take(max)
                .partition(|m| m.body.len() as u64 > max_size_bytes);
            let outcomes = wanted
                .into_iter()
                .map(|m| {
                    if self.fail_uids.contains(&m.uid) {
                        FetchOutcome::Failed {
                            uid: m.uid,
                            reason: "synthetic source fetch failure".into(),
                        }
                    } else {
                        FetchOutcome::Fetched(Box::new(m))
                    }
                })
                .collect();
            let too_large = too_large
                .into_iter()
                .map(|m| UidEntry {
                    uid: m.uid,
                    size: Some(m.body.len() as u32),
                    internal_date_epoch: Some(m.internal_date_epoch),
                })
                .collect();
            Ok(FetchWindow {
                outcomes,
                too_large,
            })
        }
        async fn enumerate_window(
            &self,
            mailbox: &str,
            cursor: Option<MailboxCursor>,
            max: usize,
        ) -> Result<Vec<UidEntry>, NestError> {
            self.enumerated.lock().unwrap().push(mailbox.to_string());
            let from = cursor.map_or(1, |c| c.last_processed_source_uid + 1);
            Ok(self
                .messages
                .get(mailbox)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|m| m.uid >= from)
                .take(max)
                .map(|m| UidEntry {
                    uid: m.uid,
                    size: Some(m.body.len() as u32),
                    internal_date_epoch: Some(m.internal_date_epoch),
                })
                .collect())
        }
        async fn logout(&self) -> Result<(), NestError> {
            *self.logged_out.lock().unwrap() = true;
            Ok(())
        }

        fn can_ever_connect(&self) -> bool {
            !self.can_never_connect
        }

        async fn replace_auth(&self, auth: SourceAuth) {
            self.auths.lock().unwrap().push(auth);
        }

        fn take_sign_in_refused(&self) -> bool {
            std::mem::take(&mut *self.sign_in_refused.lock().unwrap())
        }
    }

    /// An injectable [`RetryClock`] that records requested delays instead of
    /// waiting (`testing.md` convention 14).
    struct FakeClock {
        slept: StdMutex<Vec<u64>>,
        now: StdMutex<u64>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                slept: StdMutex::new(Vec::new()),
                now: StdMutex::new(1_000_000),
            }
        }

        fn advance(&self, ms: u64) {
            *self.now.lock().unwrap() += ms;
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl RetryClock for FakeClock {
        async fn sleep_ms(&self, ms: u64) {
            self.slept.lock().unwrap().push(ms);
            self.advance(ms);
        }

        fn now_ms(&self) -> u64 {
            *self.now.lock().unwrap()
        }
    }

    fn machine(mailboxes: &[(&str, bool, u32)]) -> MailImportMachine {
        MailImportMachine::new(
            Arc::new(FakeNest::new()),
            Arc::new(FakeSource::new(mailboxes)),
            Arc::new(FakeClock::new()),
        )
    }

    async fn connected(m: &MailImportMachine) {
        m.dispatch(MailImportAction::SetUsername {
            value: "user@example.com".into(),
        })
        .await
        .unwrap();
        m.dispatch(MailImportAction::SetPassword {
            value: "app-password".into(),
        })
        .await
        .unwrap();
        m.dispatch(MailImportAction::Connect).await.unwrap();
    }

    #[tokio::test]
    async fn hydrate_with_no_session_stays_on_source() {
        let m = machine(&[("INBOX", true, 10)]);
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.step, ImportStep::Source);
        assert_eq!(snap.source_kind, ImportSourceKind::Gmail);
        assert_eq!(snap.host, "imap.gmail.com");
        assert_eq!(snap.port, 993);
    }

    #[tokio::test]
    async fn select_source_kind_prefills_preset_host() {
        let m = machine(&[]);
        m.dispatch(MailImportAction::SelectSourceKind {
            kind: ImportSourceKind::ICloud,
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.host, "imap.mail.me.com");
        assert_eq!(snap.port, 993);
        assert_eq!(snap.tls_mode, ImportTlsMode::Implicit);

        m.dispatch(MailImportAction::SelectSourceKind {
            kind: ImportSourceKind::Generic,
        })
        .await
        .unwrap();
        // Generic has no preset — the prior preset host is left as a
        // starting point for the user to overwrite via SetHost.
        assert_eq!(m.snapshot().host, "imap.mail.me.com");
    }

    /// The `connect_actions` precondition: selecting a preset kind must overwrite a
    /// user-supplied host, since `connect_actions` never sends `SetHost` for
    /// the three presets (`Gmail`/`Outlook`/`ICloud`) and relies entirely on
    /// this arm to have run. A user
    /// who types a host under `Generic`, then switches to `Gmail` and pastes
    /// their Google app-password, must connect to Google's host — not
    /// whatever they typed first.
    #[tokio::test]
    async fn selecting_a_preset_kind_overwrites_a_user_supplied_host() {
        let m = machine(&[]);
        m.dispatch(MailImportAction::SetHost {
            value: "imap.attacker.example".into(),
        })
        .await
        .unwrap();
        m.dispatch(MailImportAction::SelectSourceKind {
            kind: ImportSourceKind::Gmail,
        })
        .await
        .unwrap();
        assert_eq!(m.snapshot().host, "imap.gmail.com");
    }

    #[tokio::test]
    async fn connect_loads_selectable_mailboxes_with_default_selection_and_clears_password() {
        let m = machine(&[
            ("INBOX", true, 100),
            ("Sent", true, 20),
            ("Trash", true, 5),
            ("[Gmail]", false, 0),
            ("[Gmail]/All Mail", true, 500),
        ]);
        connected(&m).await;
        let snap = m.snapshot();
        assert_eq!(snap.step, ImportStep::Scope);
        assert!(snap.password.is_empty(), "password cleared after connect");
        let sel: Vec<_> = snap
            .mailboxes
            .iter()
            .map(|x| (x.name.as_str(), x.selected))
            .collect();
        // "[Gmail]" is unselectable — filtered out entirely, never a row.
        assert_eq!(
            sel,
            vec![
                ("INBOX", true),
                ("Sent", true),
                ("Trash", false),
                ("[Gmail]/All Mail", false),
            ]
        );
    }

    /// § Wizard steps step 3: every discard and duplicate mailbox starts
    /// unticked — Gmail's Important and Starred too — recognised by the role
    /// the source advertises before the name, so a localized Gmail (which
    /// lists none of the English names) is read right; never hidden.
    #[tokio::test]
    async fn the_discard_and_duplicate_mailboxes_start_unticked_by_role_then_by_name() {
        let source = FakeSource::new(&[
            ("INBOX", true, 100),
            ("[Gmail]/Papierkorb", true, 5),
            ("[Gmail]/Alle Nachrichten", true, 500),
            ("[Gmail]/Markiert", true, 3),
            ("[Gmail]/Wichtig", true, 9),
            ("[Gmail]/Gesendet", true, 20),
            ("Ordner", true, 2),
            ("[Gmail]/Important", true, 9),
            ("[Gmail]/Starred", true, 3),
            ("[Gmail]/Spam", true, 1),
            ("[Google Mail]/Bin", true, 1),
            ("Junk", true, 1),
        ])
        .with_special_use("[Gmail]/Papierkorb", SpecialUse::Trash)
        .with_special_use("[Gmail]/Alle Nachrichten", SpecialUse::All)
        .with_special_use("[Gmail]/Markiert", SpecialUse::Flagged)
        .with_special_use("[Gmail]/Wichtig", SpecialUse::Important)
        .with_special_use("[Gmail]/Gesendet", SpecialUse::Sent)
        .with_special_use("Ordner", SpecialUse::Archive);
        let m = MailImportMachine::new(
            Arc::new(FakeNest::new()),
            Arc::new(source),
            Arc::new(FakeClock::new()),
        );
        connected(&m).await;
        let sel: Vec<_> = m
            .snapshot()
            .mailboxes
            .iter()
            .map(|x| (x.name.clone(), x.selected))
            .collect();
        let expect = [
            ("INBOX", true),
            ("[Gmail]/Papierkorb", false),
            ("[Gmail]/Alle Nachrichten", false),
            ("[Gmail]/Markiert", false),
            ("[Gmail]/Wichtig", false),
            ("[Gmail]/Gesendet", true),
            ("Ordner", true),
            ("[Gmail]/Important", false),
            ("[Gmail]/Starred", false),
            ("[Gmail]/Spam", false),
            ("[Google Mail]/Bin", false),
            ("Junk", false),
        ]
        .map(|(n, s)| (n.to_string(), s));
        assert_eq!(sel, expect);
    }

    /// Connect with `kind` against a source whose sign-in fails as `failure`.
    async fn failed_connect(
        kind: ImportSourceKind,
        failure: SourceConnectFailure,
        detail: &str,
    ) -> MailImportSnapshot {
        let m = MailImportMachine::new(
            Arc::new(FakeNest::new()),
            Arc::new(FakeSource::failing_connect(failure, detail)),
            Arc::new(FakeClock::new()),
        );
        m.dispatch(MailImportAction::SelectSourceKind { kind })
            .await
            .unwrap();
        m.dispatch(MailImportAction::SetUsername {
            value: "user@example.com".into(),
        })
        .await
        .unwrap();
        m.dispatch(MailImportAction::SetPassword {
            value: "typed".into(),
        })
        .await
        .unwrap();
        assert!(m.dispatch(MailImportAction::Connect).await.is_err());
        m.snapshot()
    }

    /// § Wizard steps step 2: a failed connection says what went wrong in a
    /// plain sentence plus the provider's hint — never the server's own
    /// words, which go to the log. (Outlook signs in with Microsoft, not a
    /// password: its case is `a_refused_xoauth2_connect_forgets_the_token`.)
    #[tokio::test]
    async fn a_refused_sign_in_says_so_in_plain_words_with_the_providers_hint() {
        use fauna_i18n::strings::mail_import as s;
        let raw = "NO [AUTHENTICATIONFAILED] Invalid credentials (Failure)";
        for (kind, hint) in [
            (ImportSourceKind::Gmail, s::CONNECT_HINT_GMAIL),
            (ImportSourceKind::ICloud, s::CONNECT_HINT_ICLOUD),
        ] {
            let snap = failed_connect(kind, SourceConnectFailure::SignIn, raw).await;
            assert_eq!(
                snap.error.as_deref(),
                Some(format!("{} {hint}", s::CONNECT_FAILED_SIGN_IN).as_str()),
                "{kind:?}"
            );
            assert_eq!(snap.password.as_str(), "typed", "everything typed is kept");
        }
    }

    /// The other two classes, and a generic server: one sentence each, no
    /// provider hint to give, the raw text nowhere on the surface.
    #[tokio::test]
    async fn an_unreachable_or_unsecurable_server_is_named_as_such() {
        use fauna_i18n::strings::mail_import as s;
        for (failure, sentence) in [
            (SourceConnectFailure::Reach, s::CONNECT_FAILED_REACH),
            (SourceConnectFailure::Secure, s::CONNECT_FAILED_SECURE),
            (SourceConnectFailure::SignIn, s::CONNECT_FAILED_SIGN_IN),
        ] {
            let snap = failed_connect(ImportSourceKind::Generic, failure, "raw words").await;
            assert_eq!(snap.error.as_deref(), Some(sentence), "{failure:?}");
        }
    }

    #[tokio::test]
    async fn a_failed_connect_stays_on_source_and_keeps_every_field() {
        let m = MailImportMachine::new(
            Arc::new(FakeNest::new()),
            Arc::new(FakeSource::rejecting()),
            Arc::new(FakeClock::new()),
        );
        m.dispatch(MailImportAction::SetUsername {
            value: "user@example.com".into(),
        })
        .await
        .unwrap();
        m.dispatch(MailImportAction::SetPassword {
            value: "wrong".into(),
        })
        .await
        .unwrap();
        let err = m.dispatch(MailImportAction::Connect).await;
        assert!(err.is_err());
        let snap = m.snapshot();
        assert_eq!(snap.step, ImportStep::Source, "stays on Source to retry");
        assert_eq!(
            snap.password.as_str(),
            "wrong",
            "credentials survive a failed connect so the user need not retype them"
        );
        assert!(snap.error.is_some());
    }

    /// On a seam that can never connect (web, until the relay transport
    /// lands), the retain-for-retry benefit does not exist, so a failed
    /// connect must clear the password rather than leave it resident.
    #[tokio::test]
    async fn a_never_connectable_seam_clears_the_password_on_failed_connect() {
        let m = MailImportMachine::new(
            Arc::new(FakeNest::new()),
            Arc::new(FakeSource::never_connectable()),
            Arc::new(FakeClock::new()),
        );
        m.dispatch(MailImportAction::SetUsername {
            value: "user@example.com".into(),
        })
        .await
        .unwrap();
        m.dispatch(MailImportAction::SetPassword {
            value: "typed-anyway".into(),
        })
        .await
        .unwrap();
        let err = m.dispatch(MailImportAction::Connect).await;
        assert!(err.is_err());
        let snap = m.snapshot();
        assert_eq!(snap.step, ImportStep::Source, "stays on Source to retry");
        assert!(
            snap.password.is_empty(),
            "no retry-without-retyping benefit exists here, so retention is pure cost"
        );
        assert!(snap.error.is_some());
    }

    #[tokio::test]
    async fn wizard_navigates_scope_confirm_and_back_to_source() {
        let m = machine(&[("INBOX", true, 1)]);
        connected(&m).await;
        assert_eq!(m.snapshot().step, ImportStep::Scope);
        m.dispatch(MailImportAction::Next).await.unwrap();
        assert_eq!(m.snapshot().step, ImportStep::Confirm);
        // Next past Confirm is a no-op (Start drives forward).
        m.dispatch(MailImportAction::Next).await.unwrap();
        assert_eq!(m.snapshot().step, ImportStep::Confirm);
        m.dispatch(MailImportAction::Back).await.unwrap();
        assert_eq!(m.snapshot().step, ImportStep::Scope);
        m.dispatch(MailImportAction::Back).await.unwrap();
        assert_eq!(m.snapshot().step, ImportStep::Source);
    }

    #[tokio::test]
    async fn toggle_mailbox_and_set_scope_fields() {
        let m = machine(&[("INBOX", true, 1), ("Work", true, 2)]);
        connected(&m).await;
        m.dispatch(MailImportAction::ToggleMailbox {
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

        m.dispatch(MailImportAction::SetDateFrom {
            value: "2026-01-01".into(),
        })
        .await
        .unwrap();
        m.dispatch(MailImportAction::SetMaxSizeBytes { value: 1024 })
            .await
            .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.date_from, "2026-01-01");
        assert_eq!(snap.max_size_bytes, 1024);
    }

    #[tokio::test]
    async fn start_sums_selected_mailbox_counts_into_total_and_moves_to_progress() {
        let m = machine(&[("INBOX", true, 100), ("Sent", true, 20), ("Trash", true, 5)]);
        connected(&m).await;
        // Trash starts deselected by default; INBOX + Sent = 120.
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Start).await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.step, ImportStep::Progress);
        assert_eq!(snap.session_state, Some(ImportSessionState::Running));
        assert_eq!(snap.total_count, 120);
        assert!(snap.error.is_none());
    }

    /// A refusal the nest answers to a control the user pressed is shown as
    /// the nest's own sentence, as mail settings shows its refusals — not
    /// behind the seam's `nest rejected:` wrapper. The sentence is the one a
    /// second device's Start reads (`error.bridges.import_source_locked`,
    /// `mailbox-migration.md` § Architectural rules, the per-source lock).
    #[tokio::test]
    async fn a_refused_start_shows_the_nests_own_sentence() {
        const LOCKED: &str = "This account is already being imported, here or on another of \
                              your devices. Finish or cancel that import first.";
        let m = MailImportMachine::new(
            Arc::new(FakeNest::new().queue_start_error(NestError::Rejected(LOCKED.into()))),
            Arc::new(FakeSource::new(&[("INBOX", true, 1)])),
            Arc::new(FakeClock::new()),
        );
        connected(&m).await;
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Next).await.unwrap();
        let err = m.dispatch(MailImportAction::Start).await.unwrap_err();
        assert_eq!(err.to_string(), LOCKED);
        assert_eq!(m.snapshot().error.as_deref(), Some(LOCKED));
    }

    /// "Nest unreachable" keeps its seam wording: it is not the nest's
    /// sentence, and the user needs to know the nest was never reached.
    #[tokio::test]
    async fn an_unreachable_nest_on_start_keeps_the_seam_wording() {
        let m = MailImportMachine::new(
            Arc::new(FakeNest::new().queue_start_error(NestError::Transient("ws dropped".into()))),
            Arc::new(FakeSource::new(&[("INBOX", true, 1)])),
            Arc::new(FakeClock::new()),
        );
        connected(&m).await;
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Next).await.unwrap();
        let err = m.dispatch(MailImportAction::Start).await.unwrap_err();
        assert_eq!(err.to_string(), "nest unreachable: ws dropped");
    }

    #[tokio::test]
    async fn pause_then_resume_transitions_session() {
        let m = machine(&[("INBOX", true, 1)]);
        connected(&m).await;
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Start).await.unwrap();
        m.dispatch(MailImportAction::Pause).await.unwrap();
        assert_eq!(m.snapshot().session_state, Some(ImportSessionState::Paused));
        m.dispatch(MailImportAction::Resume).await.unwrap();
        assert_eq!(
            m.snapshot().session_state,
            Some(ImportSessionState::Running)
        );
    }

    #[tokio::test]
    async fn cancel_keeps_progress_screen_with_counts_not_a_wizard_reset() {
        let m = machine(&[("INBOX", true, 1)]);
        connected(&m).await;
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Start).await.unwrap();
        m.dispatch(MailImportAction::Cancel).await.unwrap();
        let snap = m.snapshot();
        assert_eq!(
            snap.step,
            ImportStep::Progress,
            "already-imported messages are kept, so the review screen stays up"
        );
        assert_eq!(snap.session_state, Some(ImportSessionState::Cancelled));
    }

    #[tokio::test]
    async fn resume_on_hydrate_jumps_straight_to_progress() {
        let fake_nest = FakeNest::new();
        *fake_nest.session.lock().unwrap() = Some(session(ImportSessionState::Paused, 50));
        let m = MailImportMachine::new(
            Arc::new(fake_nest),
            Arc::new(FakeSource::new(&[])),
            Arc::new(FakeClock::new()),
        );
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.step, ImportStep::Progress);
        assert_eq!(snap.session_state, Some(ImportSessionState::Paused));
        assert_eq!(snap.total_count, 50);
    }

    /// A finished import is not resumable — page load resumes `running` and
    /// `paused` sessions only (`mailbox-migration.md` § Resume protocol) — so it
    /// must not hold the wizard on Done. It used to: after one completed import
    /// every later page load reopened on the Done screen, and the Source step a
    /// user starts the next import from was unreachable.
    #[tokio::test]
    async fn a_completed_session_does_not_hold_the_wizard_on_done() {
        let fake_nest = FakeNest::new();
        *fake_nest.session.lock().unwrap() = Some(session(ImportSessionState::Completed, 10));
        let m = MailImportMachine::new(
            Arc::new(fake_nest),
            Arc::new(FakeSource::new(&[])),
            Arc::new(FakeClock::new()),
        );
        m.hydrate().await.unwrap();
        assert_eq!(m.snapshot().step, ImportStep::Source);
    }

    // ── § A failed import is a record the wizard shows until the user moves on

    fn listed(id: &str, state: ImportSessionState, reason: &str) -> ImportSessionView {
        ImportSessionView {
            session_id: id.into(),
            error_reason: reason.into(),
            ..session(state, 10)
        }
    }

    /// What a fresh machine hydrated against `listing` (newest first) shows:
    /// the session it holds and the step.
    async fn adopted(listing: Vec<ImportSessionView>) -> (Option<String>, MailImportSnapshot) {
        let nest = FakeNest::new();
        *nest.listing.lock().unwrap() = Some(listing);
        let m = MailImportMachine::new(
            Arc::new(nest),
            Arc::new(FakeSource::new(&[])),
            Arc::new(FakeClock::new()),
        );
        m.hydrate().await.unwrap();
        (m.current_session_id().ok(), m.snapshot())
    }

    /// The adoption rule: the newest live session; with none, the newest
    /// `errored` one; a `completed` or `cancelled` one never.
    #[tokio::test]
    async fn refresh_adopts_the_newest_live_session_else_the_newest_failure() {
        use ImportSessionState::*;
        let (held, snap) = adopted(vec![
            listed("err", Errored, "boom"),
            listed("run", Running, ""),
        ])
        .await;
        assert_eq!(
            held.as_deref(),
            Some("run"),
            "a live import outranks a newer failure"
        );
        assert_eq!(snap.session_state, Some(Running));

        let (held, snap) = adopted(vec![
            listed("done", Completed, ""),
            listed("err", Errored, "the source refused the sign-in"),
        ])
        .await;
        assert_eq!(
            held.as_deref(),
            Some("err"),
            "a finished import is never shown"
        );
        assert_eq!(snap.step, ImportStep::Progress);
        assert_eq!(snap.session_state, Some(Errored));
        assert_eq!(snap.error_log, vec!["the source refused the sign-in"]);
        assert_eq!(snap.error, None, "the record is the log line, not an error");
        assert!(
            !snap.resume_needs_credentials,
            "a failed import has nothing to resume"
        );

        for state in [Completed, Cancelled] {
            let (held, snap) = adopted(vec![listed("x", state, "")]).await;
            assert_eq!(held, None, "{state:?}");
            assert_eq!(snap.step, ImportStep::Source, "{state:?}");
        }
    }

    /// The run's own failure: the reason is the error log's last line, after
    /// the per-message lines this app wrote, and `error-message` says it too.
    #[tokio::test]
    async fn a_failed_run_ends_its_log_with_the_reason() {
        use fauna_i18n::strings::mail_import as s;
        let total = 60u32;
        let mut source = FakeSource::new(&[("INBOX", true, total)]).with_messages("INBOX", total);
        for uid in 1..=total {
            source = source.failing_uid(uid);
        }
        let nest = Arc::new(FakeNest::new());
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;

        assert!(m.run_import().await.is_err());
        let snap = m.snapshot();
        assert_eq!(snap.session_state, Some(ImportSessionState::Errored));
        assert!(snap.error_log.len() > 1, "the per-message lines are kept");
        assert_eq!(
            snap.error_log.last().map(String::as_str),
            Some(s::ERROR_BUDGET_EXCEEDED)
        );
        assert_eq!(snap.error.as_deref(), Some(s::ERROR_BUDGET_EXCEEDED));

        // A refresh in the same app (the import's own error event) neither
        // drops the record nor writes the reason a second time.
        let lines = snap.error_log.len();
        m.dispatch(MailImportAction::Refresh).await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.step, ImportStep::Progress);
        assert_eq!(snap.error_log.len(), lines);
    }

    /// Pause, Resume and Cancel are not offered on a failed or cancelled
    /// import; should an app send one the machine refuses it with a sentence,
    /// without a call to the nest.
    #[tokio::test]
    async fn the_three_controls_are_refused_on_a_terminal_session_without_a_seam_call() {
        for state in [ImportSessionState::Errored, ImportSessionState::Cancelled] {
            let nest = Arc::new(FakeNest::new());
            *nest.session.lock().unwrap() = Some(listed("t", state, "boom"));
            let m = MailImportMachine::new(
                nest.clone(),
                Arc::new(FakeSource::new(&[])),
                Arc::new(FakeClock::new()),
            );
            m.store_session(listed("t", state, "boom"));
            for action in [
                MailImportAction::Pause,
                MailImportAction::Resume,
                MailImportAction::Cancel,
            ] {
                let err = m.dispatch(action.clone()).await.unwrap_err();
                assert!(
                    matches!(&err, DispatchError::InvalidState(msg) if !msg.is_empty()),
                    "{state:?} {action:?}: {err:?}"
                );
                assert_eq!(
                    m.snapshot().step,
                    ImportStep::Progress,
                    "{state:?} {action:?}"
                );
            }
            assert!(nest.calls().is_empty(), "{state:?}: {:?}", nest.calls());
        }
    }

    /// Dismiss deletes the record through the seam and returns the wizard to
    /// Source; no record comes back on the next load.
    #[tokio::test]
    async fn dismissing_a_failed_import_discards_it_and_returns_to_source() {
        let nest = Arc::new(FakeNest::new());
        *nest.session.lock().unwrap() = Some(listed("sess-1", ImportSessionState::Errored, "boom"));
        let m = MailImportMachine::new(
            nest.clone(),
            Arc::new(FakeSource::new(&[])),
            Arc::new(FakeClock::new()),
        );
        m.hydrate().await.unwrap();
        assert_eq!(m.snapshot().step, ImportStep::Progress);

        m.dispatch(MailImportAction::Dismiss).await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.step, ImportStep::Source);
        assert_eq!(snap.session_state, None);
        assert!(snap.error_log.is_empty());
        assert!(m.current_session_id().is_err());
        assert!(nest.calls().contains(&"discard_session".to_string()));

        m.hydrate().await.unwrap();
        assert_eq!(
            m.snapshot().step,
            ImportStep::Source,
            "no record comes back"
        );
    }

    /// Dismiss is a failed import's control only: on a live one it is refused
    /// with a sentence and the nest is not asked.
    #[tokio::test]
    async fn dismiss_is_refused_on_a_live_import() {
        let nest = Arc::new(FakeNest::new());
        let m = MailImportMachine::new(
            nest.clone(),
            Arc::new(FakeSource::new(&[])),
            Arc::new(FakeClock::new()),
        );
        m.store_session(listed("sess-1", ImportSessionState::Paused, ""));
        assert!(matches!(
            m.dispatch(MailImportAction::Dismiss).await,
            Err(DispatchError::InvalidState(_))
        ));
        assert!(nest.calls().is_empty());
        assert_eq!(m.snapshot().session_state, Some(ImportSessionState::Paused));
    }

    /// A cancelled import stays on Progress in the app session that cancelled
    /// it — a refresh keeps it — but is never adopted anywhere else.
    #[tokio::test]
    async fn a_held_cancelled_import_survives_a_refresh() {
        let m = machine(&[("INBOX", true, 1)]);
        connected(&m).await;
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Start).await.unwrap();
        m.dispatch(MailImportAction::Cancel).await.unwrap();
        m.dispatch(MailImportAction::Refresh).await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.step, ImportStep::Progress);
        assert_eq!(snap.session_state, Some(ImportSessionState::Cancelled));
    }

    /// A held failure discarded from another device is dropped at the next
    /// refresh: the wizard returns to Source instead of painting a record that
    /// no longer exists.
    #[tokio::test]
    async fn a_held_failure_gone_from_the_listing_is_dropped() {
        let nest = Arc::new(FakeNest::new());
        *nest.session.lock().unwrap() = Some(listed("sess-1", ImportSessionState::Errored, "boom"));
        let m = MailImportMachine::new(
            nest.clone(),
            Arc::new(FakeSource::new(&[])),
            Arc::new(FakeClock::new()),
        );
        m.hydrate().await.unwrap();
        *nest.session.lock().unwrap() = None;
        m.dispatch(MailImportAction::Refresh).await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.step, ImportStep::Source);
        assert_eq!(snap.session_state, None);
    }

    /// A wizard part-way through its own walk is not pulled onto another
    /// device's import by a refresh (the push re-read): past the Source step
    /// the walk in hand is the person's, and they meet the other import at
    /// Start — the per-source lock — or on their next visit.
    #[tokio::test]
    async fn a_refresh_does_not_pull_a_walk_in_progress_onto_another_import() {
        let m = machine(&[("INBOX", true, 1)]);
        connected(&m).await;
        m.dispatch(MailImportAction::Next).await.unwrap();
        assert_eq!(m.snapshot().step, ImportStep::Confirm);
        let nest = FakeNest::new();
        *nest.listing.lock().unwrap() =
            Some(vec![listed("theirs", ImportSessionState::Running, "")]);
        let m = MailImportMachine {
            nest: Arc::new(nest),
            ..m
        };
        m.dispatch(MailImportAction::Refresh).await.unwrap();
        let snap = m.snapshot();
        assert_eq!(
            snap.step,
            ImportStep::Confirm,
            "the walk stays where it was"
        );
        assert_eq!(snap.session_state, None);
        assert!(m.current_session_id().is_err());
    }

    /// A listing read before another writer moved the held view is stale:
    /// the refresh drops it rather than painting the older state over the
    /// newer one (a progress push's re-read racing the run's own pause).
    #[tokio::test]
    async fn a_refresh_whose_listing_went_stale_in_flight_changes_nothing() {
        let nest = Arc::new(FakeNest::new());
        *nest.session.lock().unwrap() = Some(listed("sess-1", ImportSessionState::Running, ""));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        *nest.list_gate.lock().unwrap() = Some((entered.clone(), release.clone()));
        let m = Arc::new(MailImportMachine::new(
            nest.clone(),
            Arc::new(FakeSource::new(&[])),
            Arc::new(FakeClock::new()),
        ));
        m.store_session(listed("sess-1", ImportSessionState::Running, ""));

        let refreshing = {
            let m = Arc::clone(&m);
            tokio::spawn(async move { m.dispatch(MailImportAction::Refresh).await })
        };
        entered.notified().await;
        // The run paused the session while the listing was on its way.
        m.store_session(listed("sess-1", ImportSessionState::Paused, ""));
        release.notify_one();
        refreshing.await.unwrap().unwrap();

        assert_eq!(m.snapshot().session_state, Some(ImportSessionState::Paused));
    }

    /// A machine holding a run ignores the nudge: its loop is the session
    /// view's writer.
    #[tokio::test]
    async fn a_refresh_leaves_the_session_of_a_held_run_alone() {
        let nest = Arc::new(FakeNest::new());
        let m = MailImportMachine::new(
            nest.clone(),
            Arc::new(FakeSource::new(&[])),
            Arc::new(FakeClock::new()),
        );
        m.store_session(listed("sess-1", ImportSessionState::Running, ""));
        *m.run_active.lock().unwrap() = true;
        *nest.listing.lock().unwrap() = Some(vec![listed(
            "other",
            ImportSessionState::Errored,
            "elsewhere",
        )]);
        m.dispatch(MailImportAction::Refresh).await.unwrap();
        assert_eq!(m.current_session_id().unwrap(), "sess-1");
        let snap = m.snapshot();
        assert_eq!(snap.session_state, Some(ImportSessionState::Running));
        assert!(snap.error_log.is_empty());
    }

    #[test]
    fn start_args_never_includes_the_password() {
        let mut snap = MailImportSnapshot::empty();
        snap.username = "user@example.com".into();
        snap.password = "super-secret".into();
        snap.mailboxes = vec![SourceMailboxOption {
            name: "INBOX".into(),
            selected: true,
            message_count: 7,
            byte_total: None,
        }];
        snap.date_from = "2023-11-14".into();
        snap.import_duplicates = true;
        let (descriptor, total, scope, date_from, skip_dedup) = snap.start_args();
        assert_eq!(descriptor, "gmail:user@example.com@imap.gmail.com");
        assert!(!descriptor.contains("super-secret"));
        assert_eq!(total, 7);
        assert_eq!(scope, vec!["INBOX".to_string()]);
        assert_eq!(date_from, "2023-11-14");
        assert!(skip_dedup);
    }

    #[test]
    fn connect_actions_for_generic_kind_sends_host_and_port() {
        let actions = connect_actions(
            ImportSourceKind::Generic,
            "  imap.example.com  ",
            "993",
            "  user@example.com  ",
            SecretString::from("pw".to_string()),
        );
        assert_eq!(
            actions,
            vec![
                MailImportAction::SetHost {
                    value: "imap.example.com".into()
                },
                MailImportAction::SetPort { value: 993 },
                MailImportAction::SetUsername {
                    value: "user@example.com".into()
                },
                MailImportAction::SetPassword {
                    value: SecretString::from("pw".to_string())
                },
                MailImportAction::Connect,
            ]
        );
    }

    #[test]
    fn connect_actions_for_gmail_omits_host_and_port() {
        // Gmail/iCloud never paint the host/port fields (`mailbox-migration.md`
        // § UX shape) — sending them anyway would commit stale drafts from a
        // provider the user never picked.
        let actions = connect_actions(
            ImportSourceKind::Gmail,
            "should-not-be-sent",
            "should-not-be-sent",
            "user@gmail.com",
            SecretString::from("app-pw".to_string()),
        );
        assert_eq!(
            actions,
            vec![
                MailImportAction::SetUsername {
                    value: "user@gmail.com".into()
                },
                MailImportAction::SetPassword {
                    value: SecretString::from("app-pw".to_string())
                },
                MailImportAction::Connect,
            ]
        );
    }

    /// Outlook is a preset like Gmail and iCloud since the 2026-10-09 ruling
    /// (`mailbox-migration.md` § Microsoft sign-in — the device-code grant):
    /// the password fallback's host field is withdrawn, so a stale draft
    /// must never replace the preset's `outlook.office365.com`.
    #[test]
    fn connect_actions_for_outlook_omits_host_and_port() {
        let actions = connect_actions(
            ImportSourceKind::Outlook,
            "  imap.outlook-fallback.example  ",
            "993",
            "  user@outlook.example  ",
            SecretString::default(),
        );
        assert_eq!(
            actions,
            vec![
                MailImportAction::SetUsername {
                    value: "user@outlook.example".into()
                },
                MailImportAction::SetPassword {
                    value: SecretString::default()
                },
                MailImportAction::Connect,
            ]
        );
    }

    /// The Outlook button commits the trimmed address before asking for a
    /// code, so `begin_microsoft_sign_in` reads the user the field shows.
    #[test]
    fn microsoft_sign_in_actions_commit_the_address_then_begin() {
        assert_eq!(
            microsoft_sign_in_actions("  user@outlook.example  "),
            vec![
                MailImportAction::SetUsername {
                    value: "user@outlook.example".into()
                },
                MailImportAction::BeginMicrosoftSignIn,
            ]
        );
    }

    #[test]
    fn connect_actions_drops_an_unparseable_port() {
        let actions = connect_actions(
            ImportSourceKind::Generic,
            "imap.example.com",
            "not-a-port",
            "user",
            SecretString::from("pw".to_string()),
        );
        assert_eq!(
            actions,
            vec![
                MailImportAction::SetHost {
                    value: "imap.example.com".into()
                },
                MailImportAction::SetUsername {
                    value: "user".into()
                },
                MailImportAction::SetPassword {
                    value: SecretString::from("pw".to_string())
                },
                MailImportAction::Connect,
            ]
        );
    }

    #[tokio::test]
    async fn start_records_the_selected_scope_on_the_session() {
        // mailbox-migration.md § Resume protocol: the session the machine
        // opens via Start must carry the scope it was opened with, so a
        // later `list_import_sessions` (resume) can read it back.
        let nest = Arc::new(FakeNest::new());
        let m = MailImportMachine::new(
            nest.clone(),
            Arc::new(FakeSource::new(&[
                ("INBOX", true, 10),
                ("Work", true, 5),
                ("Trash", true, 2),
            ])),
            Arc::new(FakeClock::new()),
        );
        connected(&m).await;
        // Trash starts deselected by default (§ Wizard steps step 3) —
        // left untouched, so the recorded scope must exclude it.
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Start).await.unwrap();
        assert_eq!(m.snapshot().total_count, 15);

        let stored = nest.session.lock().unwrap().clone().unwrap();
        assert_eq!(
            stored.scope,
            vec!["INBOX".to_string(), "Work".to_string()],
            "Trash was deselected, so it must not be part of the recorded scope"
        );
    }

    // ── run_import — the fetch-drive loop ──────────────────────────────

    /// Connects, advances past Scope/Confirm, and `Start`s — leaves the
    /// machine `Running` with a real nest-side session, ready for
    /// `run_import`.
    async fn started(
        nest: Arc<FakeNest>,
        source: FakeSource,
        clock: Arc<FakeClock>,
    ) -> MailImportMachine {
        started_with_source(nest, Arc::new(source), clock).await
    }

    /// [`started`], keeping a handle on the source to read what it was asked.
    async fn started_with_source(
        nest: Arc<FakeNest>,
        source: Arc<FakeSource>,
        clock: Arc<FakeClock>,
    ) -> MailImportMachine {
        let m = MailImportMachine::new(nest, source, clock);
        connected(&m).await;
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Start).await.unwrap();
        m
    }

    #[tokio::test]
    async fn run_import_happy_path_imports_every_message_and_finalizes() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3)]).with_messages("INBOX", 3);
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;

        m.run_import().await.unwrap();

        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 2, 3]);
        let snap = m.snapshot();
        assert_eq!(snap.imported_count, 3);
        assert_eq!(snap.errored_count, 0);
        assert_eq!(snap.error, None);
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed
        );
    }

    #[tokio::test]
    async fn run_import_walks_every_selected_mailbox_in_order() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 2), ("Work", true, 2)])
            .with_messages("INBOX", 2)
            .with_messages("Work", 2);
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;

        m.run_import().await.unwrap();

        // Both mailboxes fully imported (2 + 2 = 4), INBOX before Work — the
        // wizard's own mailbox list order (§ Wizard steps step 3), and each
        // mailbox's own uids ascend, since `sent_uids` is append-order.
        assert_eq!(nest.sent_uids.lock().unwrap().len(), 4);
        assert_eq!(m.snapshot().imported_count, 4);
    }

    /// § Wizard steps step 5 ("Per-mailbox: list with per-mailbox
    /// progress"): Start lays one row per selected mailbox at nothing yet,
    /// and the drive loop counts each outcome against the mailbox it came
    /// from — a too-large skip included — with the row's text from the one
    /// shared formatter.
    #[tokio::test]
    async fn each_selected_mailbox_counts_its_own_progress() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3), ("Work", true, 2)])
            .with_messages("INBOX", 3)
            .with_messages("Work", 2)
            .sized("INBOX", 2, 4_096, 0);
        let m = MailImportMachine::new(nest.clone(), Arc::new(source), Arc::new(FakeClock::new()));
        connected(&m).await;
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::SetMaxSizeBytes { value: 1_024 })
            .await
            .unwrap();
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Start).await.unwrap();
        let row = |name: &str, imported, skipped, total| ImportMailboxProgress {
            name: name.into(),
            imported,
            skipped,
            errored: 0,
            total,
        };
        assert_eq!(
            m.snapshot().mailbox_progress,
            vec![row("INBOX", 0, 0, 3), row("Work", 0, 0, 2)],
            "Start lays a row per selected mailbox, none of it done yet"
        );

        m.run_import().await.unwrap();

        let rows = m.snapshot().mailbox_progress;
        assert_eq!(rows, vec![row("INBOX", 2, 1, 3), row("Work", 2, 0, 2)]);
        assert_eq!(
            import_mailbox_progress_text(rows[0].clone()),
            LocalizedText::key_args(
                "mail_import.progress_row_counts_fmt",
                [
                    ("imported", "2"),
                    ("count", "3"),
                    ("skipped", "1"),
                    ("errored", "0")
                ]
            )
        );
    }

    #[tokio::test]
    async fn run_import_with_no_selected_mailboxes_finalizes_immediately() {
        // Every mailbox toggled off — a legal (if pointless) wizard path.
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3)]).with_messages("INBOX", 3);
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;
        m.dispatch(MailImportAction::ToggleMailbox {
            mailbox: "INBOX".into(),
        })
        .await
        .unwrap();

        m.run_import().await.unwrap();

        assert!(nest.sent_uids.lock().unwrap().is_empty());
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed
        );
    }

    #[tokio::test]
    async fn a_pause_dispatched_before_run_import_stops_it_at_the_first_checkpoint() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3)]).with_messages("INBOX", 3);
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;

        m.dispatch(MailImportAction::Pause).await.unwrap();
        m.run_import().await.unwrap();

        assert!(
            nest.sent_uids.lock().unwrap().is_empty(),
            "the checkpoint at the top of the loop must fire before any fetch"
        );
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Paused,
            "run_import must not finalize a paused session"
        );
    }

    /// § Wizard steps step 5 "Pause persists; resume picks up where it left
    /// off", and the loop's own contract that a Pause drains the window in
    /// flight: a Pause pressed while a window is being fetched hands that
    /// window to the nest and only then pauses the session — the nest takes
    /// messages only into a running session, so pausing it first refused
    /// the drain and the window's messages never arrived.
    #[tokio::test]
    async fn a_pause_during_a_fetch_window_drains_it_before_pausing_the_session() {
        let nest = Arc::new(FakeNest::new());
        let (source, reached, release) = FakeSource::new(&[("INBOX", true, 3), ("Work", true, 2)])
            .with_messages("INBOX", 3)
            .with_messages("Work", 2)
            .gating_fetch("INBOX");
        let m = Arc::new(started(nest.clone(), source, Arc::new(FakeClock::new())).await);

        let run = tokio::spawn({
            let m = m.clone();
            async move { m.run_import().await }
        });
        reached.notified().await;
        m.dispatch(MailImportAction::Pause).await.unwrap();
        release.notify_one();
        run.await.unwrap().unwrap();

        assert_eq!(
            nest.sent_uids.lock().unwrap().as_slice(),
            &[1, 2, 3],
            "the window in flight reaches the nest, and Work is not started"
        );
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Paused
        );
        let snap = m.snapshot();
        assert_eq!(snap.session_state, Some(ImportSessionState::Paused));
        assert_eq!(snap.imported_count, 3);
        assert_eq!(snap.error, None, "a Pause is not an error");

        m.dispatch(MailImportAction::Resume).await.unwrap();
        m.run_import().await.unwrap();
        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 2, 3, 1, 2]);
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed
        );
    }

    /// A Resume pressed before the running loop reached its pause withdraws
    /// the request: the nest session never stopped, so nothing is asked of
    /// it, and the run carries on to the end.
    #[tokio::test]
    async fn a_resume_before_the_window_drains_withdraws_the_pause() {
        let nest = Arc::new(FakeNest::new());
        let (source, reached, release) = FakeSource::new(&[("INBOX", true, 3), ("Work", true, 2)])
            .with_messages("INBOX", 3)
            .with_messages("Work", 2)
            .gating_fetch("INBOX");
        let m = Arc::new(started(nest.clone(), source, Arc::new(FakeClock::new())).await);

        let run = tokio::spawn({
            let m = m.clone();
            async move { m.run_import().await }
        });
        reached.notified().await;
        m.dispatch(MailImportAction::Pause).await.unwrap();
        m.dispatch(MailImportAction::Resume).await.unwrap();
        release.notify_one();
        run.await.unwrap().unwrap();

        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 2, 3, 1, 2]);
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed
        );
        let calls = nest.outbound.lock().unwrap();
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("pause_session") || c.starts_with("resume_session")),
            "the nest was asked to pause or resume: {calls:?}"
        );
    }

    #[tokio::test]
    async fn a_cancel_dispatched_before_run_import_stops_it_at_the_first_checkpoint() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3)]).with_messages("INBOX", 3);
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;

        m.dispatch(MailImportAction::Cancel).await.unwrap();
        m.run_import().await.unwrap();

        assert!(nest.sent_uids.lock().unwrap().is_empty());
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Cancelled
        );
    }

    #[tokio::test]
    async fn a_second_run_import_call_resumes_from_the_tracked_cursor_not_from_scratch() {
        // § Resume protocol, same-process case: INBOX fully imports on the
        // first call; Work exhausts its retries and the loop pauses the
        // session. The Resume's `run_import` call (no fresh `Start`) must not
        // re-send INBOX's already-sent messages — only its own cursor,
        // tracked on the machine, prevents that.
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3), ("Work", true, 2)])
            .with_messages("INBOX", 3)
            .with_messages("Work", 2)
            .queue_fetch_error("Work", NestError::Transient("t1".into()))
            .queue_fetch_error("Work", NestError::Transient("t2".into()))
            .queue_fetch_error("Work", NestError::Transient("t3".into()))
            .queue_fetch_error("Work", NestError::Transient("t4".into()));
        let clock = Arc::new(FakeClock::new());
        let m = started(nest.clone(), source, clock.clone()).await;

        m.run_import().await.unwrap();
        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 2, 3]);
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Paused
        );
        // 3 retries (the 4th queued failure is never reached — the 3rd
        // exhausts RETRY_DELAYS_MS).
        assert_eq!(clock.slept.lock().unwrap().as_slice(), &RETRY_DELAYS_MS);

        // The person resumes; Work's error queue is now drained, so this call
        // succeeds — and must not re-send INBOX's 3 messages.
        m.dispatch(MailImportAction::Resume).await.unwrap();
        m.run_import().await.unwrap();
        assert_eq!(
            nest.sent_uids.lock().unwrap().as_slice(),
            &[1, 2, 3, 1, 2],
            "INBOX's uids 1-3 must not reappear; Work's own 1-2 are new"
        );
    }

    #[tokio::test]
    async fn a_source_fetch_failure_is_logged_and_counted_without_stopping_the_run() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3)])
            .with_messages("INBOX", 3)
            .failing_uid(2);
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;

        m.run_import().await.unwrap();

        // uid 2 never reached nest at all (a source-side failure).
        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 3]);
        let snap = m.snapshot();
        assert!(
            snap.error_log.iter().any(|l| l.contains("uid 2")),
            "{:?}",
            snap.error_log
        );
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed,
            "one failure is far under both budget thresholds"
        );
    }

    /// § Wizard steps steps 5–6: a message the NEST skips or errors is listed
    /// in the skip log with its mailbox, uid and reason — not only the ones
    /// the client decides itself — and the log is still there once the run
    /// reaches Done, where "Review skipped" shows it.
    #[tokio::test]
    async fn nest_side_skips_and_errors_are_logged_and_the_log_survives_done() {
        let nest = Arc::new(FakeNest::new().with_dedup_uids([2]).with_error_uids([3]));
        let source = FakeSource::new(&[("INBOX", true, 3)]).with_messages("INBOX", 3);
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;

        m.run_import().await.unwrap();

        let snap = m.snapshot();
        assert_eq!(snap.step, ImportStep::Done, "{snap:?}");
        assert_eq!(
            snap.error_log,
            vec![
                "INBOX uid 2: dedup".to_string(),
                "INBOX uid 3: synthetic test failure".to_string(),
            ]
        );
    }

    /// § Per-message flow step 1: a message over the user's own size limit is
    /// never fetched or sent; it is listed as too large and reaches the
    /// nest's `skipped_count` (§ Progress lives nest-side), while the rest of
    /// the mailbox imports.
    #[tokio::test]
    async fn a_message_over_the_users_size_limit_is_skipped_as_too_large() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3)])
            .with_messages("INBOX", 3)
            .sized("INBOX", 2, 4_096, 0);
        let m = MailImportMachine::new(nest.clone(), Arc::new(source), Arc::new(FakeClock::new()));
        connected(&m).await;
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::SetMaxSizeBytes { value: 1_024 })
            .await
            .unwrap();
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Start).await.unwrap();

        m.run_import().await.unwrap();

        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 3]);
        let recorded = nest.recorded.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![(
                "INBOX".to_string(),
                vec![ClientImportOutcome {
                    uid: 2,
                    kind: ClientImportOutcomeKind::TooLarge { size: 4_096 },
                    extra: Default::default(),
                }]
            )]
        );
        let snap = m.snapshot();
        assert_eq!((snap.imported_count, snap.skipped_count), (2, 1));
        assert!(
            snap.error_log
                .iter()
                .any(|l| l.contains("uid 2") && l.contains("too large")),
            "{:?}",
            snap.error_log
        );
    }

    /// Scope before size (§ Wizard steps step 3): a too-large message the
    /// since date already excludes is out of scope — never counted, never
    /// reported — and comes off the estimate like any other excluded one.
    #[tokio::test]
    async fn a_too_large_message_outside_the_date_range_is_not_counted() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 2)])
            .with_dated_messages("INBOX", &[(1, 1_700_000_000), (2, 1_000)])
            .sized("INBOX", 2, 4_096, 1_000);
        let m = MailImportMachine::new(nest.clone(), Arc::new(source), Arc::new(FakeClock::new()));
        connected(&m).await;
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::SetMaxSizeBytes { value: 1_024 })
            .await
            .unwrap();
        m.dispatch(MailImportAction::SetDateFrom {
            value: "2020-01-01".into(),
        })
        .await
        .unwrap();
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Start).await.unwrap();

        m.run_import().await.unwrap();

        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1]);
        assert!(nest.recorded.lock().unwrap().is_empty());
        assert_eq!(m.snapshot().skipped_count, 0);
    }

    /// § Failure handling → *Source-side errors*: a message the source will
    /// not hand over is counted as failed on the session row, not only logged.
    #[tokio::test]
    async fn a_source_fetch_failure_reaches_the_sessions_failed_count() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3)])
            .with_messages("INBOX", 3)
            .failing_uid(2);
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;

        m.run_import().await.unwrap();

        let recorded = nest.recorded.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1);
        assert!(matches!(
            recorded[0].1.as_slice(),
            [ClientImportOutcome {
                uid: 2,
                kind: ClientImportOutcomeKind::SourceUnreadable { .. },
                ..
            }]
        ));
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().errored_count,
            1
        );
        assert_eq!(m.snapshot().errored_count, 1);
    }

    /// A started machine whose INBOX cursor was recorded under UIDVALIDITY 6
    /// while the source now reports the fake's 7 — the source rebuilt INBOX
    /// after the import began (§ Resume protocol step 4).
    async fn started_with_a_rebuilt_inbox(nest: Arc<FakeNest>) -> MailImportMachine {
        let source = FakeSource::new(&[("INBOX", true, 3), ("Work", true, 2)])
            .with_messages("INBOX", 3)
            .with_messages("Work", 2);
        let m = started(nest, source, Arc::new(FakeClock::new())).await;
        m.cursors.lock().unwrap().insert(
            "INBOX".into(),
            MailboxCursor {
                last_processed_source_uid: 1,
                source_uid_validity: 6,
            },
        );
        m
    }

    /// § Resume protocol step 4: a changed UIDVALIDITY is per-mailbox — the
    /// other mailboxes carry on — but the session neither completes nor
    /// imports against the new UID space: it pauses with the warning, and the
    /// rebuilt mailbox is what the restart offer is for.
    #[tokio::test]
    async fn a_uidvalidity_mismatch_warns_skips_the_mailbox_and_pauses_for_a_restart() {
        let nest = Arc::new(FakeNest::new());
        let m = started_with_a_rebuilt_inbox(nest.clone()).await;

        m.run_import().await.unwrap();

        // INBOX not walked at all; Work fully imports.
        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 2]);
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Paused,
            "a pending rebuilt mailbox keeps the session on the progress step"
        );
        let snap = m.snapshot();
        assert_eq!(snap.rebuilt_mailboxes, vec!["INBOX".to_string()]);
        assert_eq!(snap.pause_reason, Some(ImportPauseReason::MailboxRebuilt));
        assert_eq!(
            snap.error.as_deref(),
            Some(fauna_i18n::strings::mail_import::PAUSED_MAILBOX_REBUILT)
        );
        assert_eq!(
            snap.error_log,
            vec![fauna_i18n::strings::mail_import::mailbox_rebuilt_fmt(
                "INBOX"
            )],
            "the log carries the warning, never the source's raw words"
        );
    }

    /// The restart offer: the rebuilt mailbox's cursor is dropped and the
    /// walk takes it again from its first message (the nest's dedup skips
    /// what already arrived, § Dedup vs. existing mail), and the session then
    /// completes.
    #[tokio::test]
    async fn restarting_a_rebuilt_mailbox_walks_it_from_its_first_message() {
        let nest = Arc::new(FakeNest::new());
        let m = started_with_a_rebuilt_inbox(nest.clone()).await;
        m.run_import().await.unwrap();

        m.dispatch(MailImportAction::RestartRebuiltMailboxes)
            .await
            .unwrap();
        let snap = m.snapshot();
        assert!(snap.rebuilt_mailboxes.is_empty());
        assert_eq!(snap.pause_reason, None);
        assert_eq!(snap.session_state, Some(ImportSessionState::Running));

        m.run_import().await.unwrap();
        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 2, 1, 2, 3]);
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed
        );
    }

    /// A plain Resume while the rebuilt mailbox is still pending does not
    /// import it either: the run pauses on it again, and the warning is not
    /// written twice.
    #[tokio::test]
    async fn resuming_past_a_pending_rebuilt_mailbox_pauses_on_it_again() {
        let nest = Arc::new(FakeNest::new());
        let m = started_with_a_rebuilt_inbox(nest.clone()).await;
        m.run_import().await.unwrap();

        m.dispatch(MailImportAction::Resume).await.unwrap();
        m.run_import().await.unwrap();

        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 2]);
        let snap = m.snapshot();
        assert_eq!(snap.session_state, Some(ImportSessionState::Paused));
        assert_eq!(snap.rebuilt_mailboxes, vec!["INBOX".to_string()]);
        assert_eq!(snap.error_log.len(), 1);
    }

    #[tokio::test]
    async fn a_restart_with_no_rebuilt_mailbox_is_refused() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3)]).with_messages("INBOX", 3);
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;

        assert!(
            m.dispatch(MailImportAction::RestartRebuiltMailboxes)
                .await
                .is_err()
        );
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Running
        );
    }

    /// A mailbox the source refuses to `EXAMINE` (it vanished) is still noted
    /// and passed over without holding the session: only a rebuilt one waits
    /// for the person.
    #[tokio::test]
    async fn a_mailbox_the_source_refuses_to_examine_is_skipped_and_the_session_completes() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3), ("Work", true, 2)])
            .with_messages("INBOX", 3)
            .with_messages("Work", 2)
            .queue_examine_error("INBOX", NestError::Rejected("NO no such mailbox".into()));
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;

        m.run_import().await.unwrap();

        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 2]);
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed
        );
        assert!(m.snapshot().rebuilt_mailboxes.is_empty());
    }

    #[tokio::test]
    async fn a_transient_source_error_retries_then_succeeds() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3)])
            .with_messages("INBOX", 3)
            .queue_examine_error("INBOX", NestError::Transient("blip".into()));
        let clock = Arc::new(FakeClock::new());
        let m = started(nest.clone(), source, clock.clone()).await;

        m.run_import().await.unwrap();

        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 2, 3]);
        assert_eq!(
            clock.slept.lock().unwrap().as_slice(),
            &[RETRY_DELAYS_MS[0]],
            "exactly one retry delay for the one transient failure"
        );
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed
        );
    }

    /// § Failure handling → *Source-side errors* (ruled 2026-10-01): a
    /// network failure that outlasts its three retries PAUSES the session with
    /// the reason shown, never `errored` — which nothing can resume — and the
    /// person resumes from where it stopped once the network is back.
    #[tokio::test]
    async fn a_lost_source_connection_that_outlasts_its_retries_pauses_and_resumes() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3)])
            .with_messages("INBOX", 3)
            .queue_examine_error("INBOX", NestError::Transient("t1".into()))
            .queue_examine_error("INBOX", NestError::Transient("t2".into()))
            .queue_examine_error("INBOX", NestError::Transient("t3".into()))
            .queue_examine_error("INBOX", NestError::Transient("t4".into()));
        let clock = Arc::new(FakeClock::new());
        let m = started(nest.clone(), source, clock.clone()).await;

        m.run_import().await.unwrap();

        assert!(nest.sent_uids.lock().unwrap().is_empty());
        assert_eq!(clock.slept.lock().unwrap().as_slice(), &RETRY_DELAYS_MS);
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Paused,
            "paused, never errored"
        );
        let snap = m.snapshot();
        assert_eq!(snap.session_state, Some(ImportSessionState::Paused));
        assert_eq!(snap.pause_reason, Some(ImportPauseReason::ConnectionLost));
        assert!(snap.error.is_some(), "the reason is shown");

        m.dispatch(MailImportAction::Resume).await.unwrap();
        assert_eq!(
            m.snapshot().pause_reason,
            None,
            "a resume clears the reason"
        );
        m.run_import().await.unwrap();
        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 2, 3]);
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed
        );
    }

    /// § Failure handling → *Nest-side errors*: a dropped connection to the
    /// nest is retried like the source's, not sent straight to the failure
    /// path.
    #[tokio::test]
    async fn a_failed_send_to_the_nest_is_retried_then_succeeds() {
        let nest =
            Arc::new(FakeNest::new().queue_send_error(NestError::Transient("ws dropped".into())));
        let source = FakeSource::new(&[("INBOX", true, 3)]).with_messages("INBOX", 3);
        let clock = Arc::new(FakeClock::new());
        let m = started(nest.clone(), source, clock.clone()).await;

        m.run_import().await.unwrap();

        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 2, 3]);
        assert_eq!(
            clock.slept.lock().unwrap().as_slice(),
            &[RETRY_DELAYS_MS[0]]
        );
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed
        );
    }

    /// The nest half of the same rule: a send that outlasts its retries
    /// pauses, and the resume sends the window again — nothing it held is
    /// lost.
    #[tokio::test]
    async fn a_lost_nest_connection_that_outlasts_its_retries_pauses_and_resumes() {
        let nest = Arc::new(
            FakeNest::new()
                .queue_send_error(NestError::Transient("t1".into()))
                .queue_send_error(NestError::Transient("t2".into()))
                .queue_send_error(NestError::Transient("t3".into()))
                .queue_send_error(NestError::Transient("t4".into())),
        );
        let source = FakeSource::new(&[("INBOX", true, 3)]).with_messages("INBOX", 3);
        let clock = Arc::new(FakeClock::new());
        let m = started(nest.clone(), source, clock.clone()).await;

        m.run_import().await.unwrap();

        assert_eq!(clock.slept.lock().unwrap().as_slice(), &RETRY_DELAYS_MS);
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Paused
        );
        assert_eq!(
            m.snapshot().pause_reason,
            Some(ImportPauseReason::ConnectionLost)
        );

        m.dispatch(MailImportAction::Resume).await.unwrap();
        m.run_import().await.unwrap();
        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 2, 3]);
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed
        );
    }

    /// § Quota composition / § Failure handling → *Nest-side errors*: on the
    /// FIRST `quota_exceeded` answer the client pauses the session and asks —
    /// it never walks on through the rest of the source skipping every
    /// message. The resume (once there is room) takes up from the first
    /// message the mailbox had no room for.
    #[tokio::test]
    async fn a_full_mailbox_pauses_on_the_first_quota_answer_and_resumes_from_it() {
        let total = FETCH_WINDOW_SIZE as u32 + 8;
        let nest = Arc::new(FakeNest::new().with_quota_uids(5..=total));
        let source = FakeSource::new(&[("INBOX", true, total)]).with_messages("INBOX", total);
        let clock = Arc::new(FakeClock::new());
        let m = started(nest.clone(), source, clock).await;

        m.run_import().await.unwrap();

        let sent = nest.sent_uids.lock().unwrap().clone();
        assert!(
            sent.iter().all(|&u| u <= FETCH_WINDOW_SIZE as u32),
            "no window after the full one was sent: {sent:?}"
        );
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Paused
        );
        let snap = m.snapshot();
        assert_eq!(snap.pause_reason, Some(ImportPauseReason::MailboxFull));
        assert!(snap.error.is_some(), "the reason is shown");

        // The admin made room.
        nest.quota_uids.lock().unwrap().clear();
        nest.sent_uids.lock().unwrap().clear();
        m.dispatch(MailImportAction::Resume).await.unwrap();
        m.run_import().await.unwrap();
        let resent = nest.sent_uids.lock().unwrap().clone();
        assert_eq!(resent, (5..=total).collect::<Vec<_>>());
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed
        );
    }

    /// § Failure handling → *Nest-side errors*, "skip remaining": from the
    /// full-mailbox pause, every message the import has not taken yet — the
    /// turned-away one first — is recorded as a plain skip, never
    /// `quota_exceeded` (so the nest's quota hold lifts, § Progress lives
    /// nest-side → *The quota hold*), no body is fetched for it, each is
    /// counted once, and the session finishes.
    #[tokio::test]
    async fn skip_the_rest_records_plain_skips_from_the_turned_away_message_and_finishes() {
        let total = FETCH_WINDOW_SIZE as u32 + 8;
        let nest = Arc::new(FakeNest::new().with_quota_uids(5..=total));
        let source =
            Arc::new(FakeSource::new(&[("INBOX", true, total)]).with_messages("INBOX", total));
        let m = started_with_source(nest.clone(), source.clone(), Arc::new(FakeClock::new())).await;
        m.run_import().await.unwrap();
        assert_eq!(
            m.snapshot().pause_reason,
            Some(ImportPauseReason::MailboxFull)
        );
        let fetches_before = source.fetched.lock().unwrap().len();
        let sent_before = nest.sent_uids.lock().unwrap().len();

        m.dispatch(MailImportAction::SkipRest).await.unwrap();
        assert!(import_action_starts_run(MailImportAction::SkipRest));
        m.run_import().await.unwrap();

        let recorded: Vec<ClientImportOutcome> = nest
            .recorded
            .lock()
            .unwrap()
            .iter()
            .flat_map(|(_, o)| o.clone())
            .collect();
        assert_eq!(
            recorded.iter().map(|o| o.uid).collect::<Vec<_>>(),
            (5..=total).collect::<Vec<_>>(),
            "from the turned-away message to the end"
        );
        assert!(
            recorded
                .iter()
                .all(|o| o.kind == ClientImportOutcomeKind::SkippedByUser),
            "a plain skip, never a quota refusal: {recorded:?}"
        );
        assert_eq!(
            source.fetched.lock().unwrap().len(),
            fetches_before,
            "no body fetched"
        );
        assert_eq!(
            nest.sent_uids.lock().unwrap().len(),
            sent_before,
            "nothing sent"
        );
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed
        );
        let snap = m.snapshot();
        assert_eq!(
            snap.skipped_count,
            u64::from(total - 4),
            "each counted once"
        );
        assert_eq!(snap.imported_count, 4);
        assert_eq!(snap.pause_reason, None);
    }

    /// Skip the rest is a way on from a pause; a running import refuses it.
    #[tokio::test]
    async fn skip_the_rest_needs_a_paused_import() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3)]).with_messages("INBOX", 3);
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;
        assert!(m.dispatch(MailImportAction::SkipRest).await.is_err());
        assert!(nest.recorded.lock().unwrap().is_empty());
    }

    /// The one list of run-starting actions every app reads.
    #[test]
    fn the_run_starting_actions_include_skip_rest() {
        for a in [
            MailImportAction::Start,
            MailImportAction::Resume,
            MailImportAction::RestartRebuiltMailboxes,
            MailImportAction::SkipRest,
        ] {
            assert!(import_action_starts_run(a));
        }
        for a in [
            MailImportAction::Pause,
            MailImportAction::Cancel,
            MailImportAction::Connect,
        ] {
            assert!(!import_action_starts_run(a));
        }
    }

    #[tokio::test]
    async fn a_fractional_error_budget_breach_fails_the_session() {
        // § Per-message error budget: "more than 10% … in the most recent
        // 1000-message window." Spaced 9 apart (no run near 51, isolating
        // the fraction condition) and past ERROR_BUDGET_MIN_SAMPLE (the
        // implementation's floor against a false positive on a tiny
        // mailbox, see that const's doc) before the ratio can matter.
        let error_uids: HashSet<u32> = (10..=100).step_by(9).collect();
        assert!(error_uids.len() > 1);
        let nest = Arc::new(FakeNest::new().with_error_uids(error_uids));
        let source = FakeSource::new(&[("INBOX", true, 100)]).with_messages("INBOX", 100);
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;

        let result = m.run_import().await;

        assert!(result.is_err());
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Errored
        );
    }

    #[tokio::test]
    async fn fifty_one_consecutive_errors_fail_the_session_even_under_the_fraction_floor() {
        // § Per-message error budget's second, independent threshold: "more
        // than 50 consecutive." 459 successes keep the running fraction at
        // 51 / 510 ≈ 10.0% — at, not over — so only the consecutive-run
        // check can be what trips this.
        let total = 510u32;
        let error_uids: HashSet<u32> = (460..=510).collect();
        assert_eq!(error_uids.len(), 51);
        let nest = Arc::new(FakeNest::new().with_error_uids(error_uids));
        let source = FakeSource::new(&[("INBOX", true, total)]).with_messages("INBOX", total);
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;

        let result = m.run_import().await;

        assert!(result.is_err());
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Errored
        );
        // The breach is caught inside the window it occurs in — no message
        // past the consecutive run's end (which lands inside the 16th
        // window: ceil(510/32) windows total) was ever sent.
        assert!(nest.sent_uids.lock().unwrap().len() <= 510);
    }

    /// § Per-message error budget: the session's end says why, in plain
    /// words, and the per-message reasons stay on the log.
    #[tokio::test]
    async fn a_spent_error_budget_says_why() {
        use fauna_i18n::strings::mail_import as s;
        let total = 60u32;
        let mut source = FakeSource::new(&[("INBOX", true, total)]).with_messages("INBOX", total);
        for uid in 1..=total {
            source = source.failing_uid(uid);
        }
        let nest = Arc::new(FakeNest::new());
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;

        assert!(m.run_import().await.is_err());
        let snap = m.snapshot();
        assert_eq!(snap.error.as_deref(), Some(s::ERROR_BUDGET_EXCEEDED));
        assert!(
            !snap.error_log.is_empty(),
            "the per-message reasons are kept"
        );
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().error_reason,
            s::ERROR_BUDGET_EXCEEDED
        );
    }

    /// § Failure handling → *Source-side errors*, the `LOGIN` row: a source
    /// that stops taking the password part-way through ends the session
    /// `errored`, asking for new credentials — never a skipped mailbox (the
    /// next one would be refused too) and never a retry of a password the
    /// source has stopped taking.
    #[tokio::test]
    async fn a_password_refused_mid_run_ends_the_import_asking_for_new_credentials() {
        use fauna_i18n::strings::mail_import as s;
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 2), ("Work", true, 2)])
            .with_messages("INBOX", 2)
            .with_messages("Work", 2)
            .refusing_sign_in_at_fetch("INBOX");
        let clock = Arc::new(FakeClock::new());
        let m = started(nest.clone(), source, clock.clone()).await;

        assert!(m.run_import().await.is_err());
        let snap = m.snapshot();
        assert_eq!(
            snap.error.as_deref(),
            Some(s::PASSWORD_SESSION_SIGN_IN_AGAIN)
        );
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Errored
        );
        assert!(
            nest.sent_uids.lock().unwrap().is_empty(),
            "the next mailbox is not walked"
        );
        assert!(clock.slept.lock().unwrap().is_empty(), "nothing is retried");
    }

    // ── § Wizard steps step 3 — the scope step's since date ────────────

    /// 2023-11-14 00:00:00 UTC — the same day the export twin's range tests
    /// pin, so the two surfaces' day arithmetic is compared against one
    /// constant rather than two.
    const DAY_START: i64 = 1_699_920_000;

    /// Dispatch the scope step's since field exactly as the per-app UI does.
    async fn set_since(m: &MailImportMachine, value: &str) {
        m.dispatch(MailImportAction::SetDateFrom {
            value: value.into(),
        })
        .await
        .unwrap();
    }

    /// Drive a machine to Start with a since date typed on the scope step,
    /// as every app's Next commits it (`scope_next_actions`). Mirrors
    /// `started`, which cannot take one. The first error — Next's or
    /// Start's — is the answer.
    async fn started_since(
        nest: Arc<FakeNest>,
        source: FakeSource,
        since: &str,
    ) -> Result<MailImportMachine, DispatchError> {
        let m = MailImportMachine::new(nest, Arc::new(source), Arc::new(FakeClock::new()));
        connected(&m).await;
        set_since(&m, since).await;
        m.dispatch(MailImportAction::Next).await?;
        m.dispatch(MailImportAction::Start).await.map(|()| m)
    }

    /// The whole point of the field: a user who asks for "only mail since
    /// January" gets exactly that, and is charged quota for nothing else
    /// (§ Quota composition).
    #[tokio::test]
    async fn a_since_date_imports_exactly_the_messages_on_or_after_it() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 4)]).with_dated_messages(
            "INBOX",
            &[
                (1, DAY_START - 86_400), // the day before — excluded
                (2, DAY_START),          // the day itself, first second
                (3, DAY_START + 43_200), // the day itself, midday
                (4, DAY_START + 86_400), // the day after
            ],
        );
        let m = started_since(nest.clone(), source, "2023-11-14")
            .await
            .expect("a well-formed since date starts the import");

        m.run_import().await.unwrap();

        assert_eq!(
            nest.sent_uids.lock().unwrap().as_slice(),
            &[2, 3, 4],
            "every message on or after the since day, and nothing before it"
        );
        let snap = m.snapshot();
        assert_eq!(snap.imported_count, 3);
        // An out-of-range message is outside the import's scope, NOT a skip:
        // `skipped_count` is for messages the import wanted and could not
        // take (§ Progress lives nest-side).
        assert_eq!(snap.skipped_count, 0, "out of range is not a skip");
        assert_eq!(snap.errored_count, 0);
    }

    /// The day boundary is the whole-UTC-day rule `mail-export.md`
    /// § UX shape step 2 owns, pinned to the second on this surface too.
    #[tokio::test]
    async fn the_since_days_boundary_is_pinned_to_the_second() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 2)])
            .with_dated_messages("INBOX", &[(1, DAY_START - 1), (2, DAY_START)]);
        let m = started_since(nest.clone(), source, "2023-11-14")
            .await
            .unwrap();

        m.run_import().await.unwrap();

        assert_eq!(
            nest.sent_uids.lock().unwrap().as_slice(),
            &[2],
            "the day's first second is in; one second earlier is out"
        );
    }

    /// A malformed date is refused at Scope → Confirm, before any session
    /// exists — never "no range", which would silently import the whole mailbox.
    #[tokio::test]
    async fn a_malformed_since_date_is_refused_before_any_session_opens() {
        for bad in ["2023-13-01", "2023-11-31", "14/11/2023", "2023-11", "nope"] {
            let nest = Arc::new(FakeNest::new());
            let source = FakeSource::new(&[("INBOX", true, 1)]).with_messages("INBOX", 1);
            let err = started_since(nest.clone(), source, bad)
                .await
                .err()
                .unwrap_or_else(|| panic!("{bad} must be refused"));
            assert!(
                matches!(err, DispatchError::InvalidState(_)),
                "{bad}: {err:?}"
            );
            assert!(
                nest.session.lock().unwrap().is_none(),
                "{bad}: no session may be opened for a date nobody can honour"
            );
        }
    }

    /// The row is the only durable record of the range (§ RPC surface's
    /// `start_import_session`), so it must reach the nest and come back.
    #[tokio::test]
    async fn the_since_date_reaches_the_session_row_and_comes_back_on_the_view() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 1)]).with_messages("INBOX", 1);
        let _m = started_since(nest.clone(), source, "2023-11-14")
            .await
            .unwrap();

        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().date_from,
            "2023-11-14",
            "recorded once at the durable commit point"
        );
    }

    /// An empty since date is unbounded — the default path, unchanged.
    #[tokio::test]
    async fn an_empty_since_date_imports_everything() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3)])
            .with_dated_messages("INBOX", &[(1, 0), (2, DAY_START), (3, DAY_START + 86_400)]);
        let m = started_since(nest.clone(), source, "").await.unwrap();

        m.run_import().await.unwrap();

        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 2, 3]);
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().date_from,
            "",
            "an unbounded import records no date"
        );
    }

    /// § Resume protocol: the resumed walk re-applies the session's date.
    /// A resume that dropped it would import every message the user excluded.
    #[tokio::test]
    async fn a_pause_and_resume_keeps_the_since_floor() {
        let nest = Arc::new(FakeNest::new());
        // Two windows' worth of messages, alternating either side of the
        // floor, so the pause lands mid-walk with excluded ones still ahead
        // of the cursor.
        let dated: Vec<(u32, i64)> = (1..=64)
            .map(|uid| {
                let epoch = if uid % 2 == 0 {
                    DAY_START
                } else {
                    DAY_START - 86_400
                };
                (uid, epoch)
            })
            .collect();
        let source = FakeSource::new(&[("INBOX", true, 64)]).with_dated_messages("INBOX", &dated);
        let m = started_since(nest.clone(), source, "2023-11-14")
            .await
            .unwrap();

        m.dispatch(MailImportAction::Pause).await.unwrap();
        m.run_import().await.unwrap();
        let after_pause = nest.sent_uids.lock().unwrap().clone();
        m.dispatch(MailImportAction::Resume).await.unwrap();
        m.run_import().await.unwrap();

        let sent = nest.sent_uids.lock().unwrap().clone();
        assert!(
            sent.len() > after_pause.len(),
            "the resume made progress: {} -> {}",
            after_pause.len(),
            sent.len()
        );
        assert!(
            sent.iter().all(|uid| uid % 2 == 0),
            "an odd uid is dated before the floor and must never be sent, \
             on the first pass or the resumed one: {sent:?}"
        );
    }

    /// The estimate must survive a Pause: the total the session opened with
    /// is the range's own count, so a resumed walk never publishes a total
    /// beneath the messages already imported.
    #[tokio::test]
    async fn a_resumed_ranged_walk_does_not_publish_a_total_below_what_it_imported() {
        let nest = Arc::new(FakeNest::new());
        // 64 messages, alternating either side of the floor, so the walk takes
        // two fetch windows and the Pause lands between them.
        let dated: Vec<(u32, i64)> = (1..=64)
            .map(|uid| {
                let epoch = if uid % 2 == 0 {
                    DAY_START
                } else {
                    DAY_START - 86_400
                };
                (uid, epoch)
            })
            .collect();
        let source = FakeSource::new(&[("INBOX", true, 64)]).with_dated_messages("INBOX", &dated);
        let m = started_since(nest.clone(), source, "2023-11-14")
            .await
            .unwrap();
        assert_eq!(
            m.snapshot().total_count,
            32,
            "starts at what the range admits"
        );

        m.dispatch(MailImportAction::Pause).await.unwrap();
        m.run_import().await.unwrap();
        let paused = m.snapshot();
        assert!(
            paused.total_count >= paused.imported_count,
            "mid-walk: total {} must never fall under imported {}",
            paused.total_count,
            paused.imported_count
        );

        m.dispatch(MailImportAction::Resume).await.unwrap();
        m.run_import().await.unwrap();

        let snap = m.snapshot();
        assert_eq!(
            snap.imported_count, 32,
            "the 32 even uids are the in-range half"
        );
        assert_eq!(
            snap.total_count, 32,
            "and the estimate stays exactly them, across the resume"
        );
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().total_count,
            32,
            "the ROW carries it too — a reload and the user's other devices \
             read the row, not this machine's snapshot"
        );
    }

    /// § Wizard steps step 3: under a since date the Confirm estimate is the
    /// range's own count from the start — never the whole mailbox's — so the
    /// session opens with the truth and the walk has nothing to revise.
    #[tokio::test]
    async fn a_since_date_estimate_counts_only_what_the_range_admits() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 4)]).with_dated_messages(
            "INBOX",
            &[
                (1, DAY_START - 86_400),
                (2, DAY_START - 86_400),
                (3, DAY_START),
                (4, DAY_START),
            ],
        );
        let m = started_since(nest.clone(), source, "2023-11-14")
            .await
            .unwrap();
        assert_eq!(
            m.snapshot().total_count,
            2,
            "the session opens on the range"
        );

        m.run_import().await.unwrap();

        assert_eq!(m.snapshot().total_count, 2, "and the walk leaves it there");
        assert!(
            nest.outbound
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.starts_with("send_unit "))
                .all(|c| c.ends_with(",null]")),
            "no revision rides a send: {:?}",
            nest.outbound.lock().unwrap()
        );
    }

    // ── The Confirm estimate (`mailbox-migration.md` § Wizard steps step 4)

    /// Scope → Confirm asks the source about each SELECTED mailbox, with the
    /// since date, and the Confirm step reads the messages and bytes.
    #[tokio::test]
    async fn next_estimates_the_selected_mailboxes_and_confirm_reads_their_totals() {
        let source = Arc::new(
            FakeSource::new(&[("INBOX", true, 0), ("Sent", true, 0), ("Trash", true, 0)])
                .with_dated_messages("INBOX", &[(1, DAY_START), (2, DAY_START - 86_400)])
                .with_messages("Sent", 2)
                .sized("INBOX", 1, 1000, DAY_START)
                .sized("Sent", 1, 300, DAY_START)
                .sized("Sent", 2, 200, DAY_START),
        );
        let m = MailImportMachine::new(
            Arc::new(FakeNest::new()),
            source.clone(),
            Arc::new(FakeClock::new()),
        );
        connected(&m).await;
        set_since(&m, "2023-11-14").await;
        m.dispatch(MailImportAction::Next).await.unwrap();

        let snap = m.snapshot();
        assert_eq!(snap.step, ImportStep::Confirm);
        assert_eq!(
            source.estimated.lock().unwrap().as_slice(),
            &[
                ("INBOX".to_string(), Some(DAY_START)),
                ("Sent".to_string(), Some(DAY_START)),
            ],
            "Trash starts unticked and is never asked about"
        );
        assert_eq!(
            snap.selected_totals(),
            ImportTotals {
                mailboxes: 2,
                messages: 3,
                bytes: Some(1500),
            }
        );
        // The binding apps' door agrees with the method.
        assert_eq!(import_selected_totals(snap.clone()), snap.selected_totals());
    }

    /// A mailbox the source will not be asked about leaves the summary with
    /// no size — never a known-looking total that silently leaves it out.
    #[tokio::test]
    async fn a_mailbox_the_source_refuses_to_estimate_leaves_the_total_unsized() {
        let source = FakeSource::new(&[("INBOX", true, 0), ("Sent", true, 0)])
            .with_messages("INBOX", 2)
            .with_messages("Sent", 1)
            .failing_estimate("Sent", NestError::Rejected("NO no such mailbox".into()));
        let m = MailImportMachine::new(
            Arc::new(FakeNest::new()),
            Arc::new(source),
            Arc::new(FakeClock::new()),
        );
        connected(&m).await;
        m.dispatch(MailImportAction::Next).await.unwrap();

        let snap = m.snapshot();
        assert_eq!(snap.step, ImportStep::Confirm, "the import is not refused");
        let totals = snap.selected_totals();
        assert_eq!(totals.messages, 2);
        assert_eq!(totals.bytes, None);
    }

    /// A dropped connection keeps the wizard on Scope with the error: the
    /// Confirm step never shows a summary nobody could compute.
    #[tokio::test]
    async fn a_connection_fault_while_estimating_stays_on_scope() {
        let source = FakeSource::new(&[("INBOX", true, 0)])
            .with_messages("INBOX", 2)
            .failing_estimate("INBOX", NestError::Transient("connection reset".into()));
        let m = MailImportMachine::new(
            Arc::new(FakeNest::new()),
            Arc::new(source),
            Arc::new(FakeClock::new()),
        );
        connected(&m).await;
        assert!(m.dispatch(MailImportAction::Next).await.is_err());

        let snap = m.snapshot();
        assert_eq!(snap.step, ImportStep::Scope);
        assert!(snap.error.is_some());
        assert_eq!(snap.status, ImportStatus::Idle);

        m.dispatch(MailImportAction::Next).await.unwrap();
        assert_eq!(m.snapshot().step, ImportStep::Confirm, "Next again retries");
    }

    #[test]
    fn selected_totals_name_no_size_until_every_selected_mailbox_has_one() {
        let mut snap = MailImportSnapshot::empty();
        snap.mailboxes = vec![
            SourceMailboxOption {
                name: "INBOX".into(),
                selected: true,
                message_count: 7,
                byte_total: Some(700),
            },
            SourceMailboxOption {
                name: "Archive".into(),
                selected: false,
                message_count: 9,
                byte_total: None,
            },
        ];
        assert_eq!(
            snap.selected_totals(),
            ImportTotals {
                mailboxes: 1,
                messages: 7,
                bytes: Some(700),
            },
            "an unselected mailbox's missing size is not this import's"
        );
        snap.mailboxes[1].selected = true;
        assert_eq!(
            snap.selected_totals(),
            ImportTotals {
                mailboxes: 2,
                messages: 16,
                bytes: None,
            }
        );
    }

    // ── The Outlook source's Microsoft sign-in (`mailbox-migration.md`
    // § Microsoft sign-in — the device-code grant) ─────────────────────────

    /// A scripted identity platform behind the machine's dyn seam.
    struct FakeIdentity {
        answers: StdMutex<VecDeque<(u16, String)>>,
        requests: StdMutex<Vec<(String, String)>>,
    }

    impl FakeIdentity {
        fn new(answers: impl IntoIterator<Item = (u16, String)>) -> Arc<Self> {
            Arc::new(Self {
                answers: StdMutex::new(answers.into_iter().collect()),
                requests: StdMutex::new(Vec::new()),
            })
        }

        fn refreshes(&self) -> usize {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, form)| form.starts_with("grant_type=refresh_token"))
                .count()
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl ImportOAuthHttp for FakeIdentity {
        async fn post_form(&self, url: &str, form: &str) -> Result<OAuthHttpResponse, String> {
            self.requests
                .lock()
                .unwrap()
                .push((url.to_string(), form.to_string()));
            let (status, body) = self
                .answers
                .lock()
                .unwrap()
                .pop_front()
                .ok_or("the scripted identity platform ran out of answers")?;
            Ok(OAuthHttpResponse {
                status,
                body: body.into(),
            })
        }
    }

    const ACCESS: &str = "at-SECRET-access-token";
    const REFRESH: &str = "rt-SECRET-refresh-token";

    fn code_answer() -> (u16, String) {
        (
            200,
            r#"{"user_code":"ABCD-EFGH","device_code":"dc-SECRET","verification_uri":"https://microsoft.com/devicelogin","expires_in":900,"interval":5}"#.into(),
        )
    }

    fn token_answer(access: &str, refresh: &str, expires_in: u64) -> (u16, String) {
        (
            200,
            format!(
                r#"{{"token_type":"Bearer","expires_in":{expires_in},"access_token":"{access}","refresh_token":"{refresh}"}}"#
            ),
        )
    }

    fn oauth_error(code: &str) -> (u16, String) {
        (400, format!(r#"{{"error":"{code}"}}"#))
    }

    fn test_identity() -> OAuthClient {
        OAuthClient {
            device_code_url: "https://idp.test/devicecode".into(),
            token_url: "https://idp.test/token".into(),
            client_id: "client-123".into(),
            scope: fauna_mail::imap_client::MICROSOFT.scope.into(),
        }
    }

    struct Outlook {
        m: MailImportMachine,
        nest: Arc<FakeNest>,
        source: Arc<FakeSource>,
        clock: Arc<FakeClock>,
        idp: Arc<FakeIdentity>,
    }

    /// A machine on the Outlook source with an address typed, its sign-in
    /// pointed at a scripted identity platform.
    async fn outlook(source: FakeSource, answers: Vec<(u16, String)>) -> Outlook {
        let nest = Arc::new(FakeNest::new());
        let source = Arc::new(source);
        let clock = Arc::new(FakeClock::new());
        let idp = FakeIdentity::new(answers);
        let m = MailImportMachine::new(nest.clone(), source.clone(), clock.clone())
            .with_oauth_http(idp.clone())
            .with_microsoft_client(test_identity());
        m.dispatch(MailImportAction::SelectSourceKind {
            kind: ImportSourceKind::Outlook,
        })
        .await
        .unwrap();
        m.dispatch(MailImportAction::SetUsername {
            value: "person@outlook.example".into(),
        })
        .await
        .unwrap();
        Outlook {
            m,
            nest,
            source,
            clock,
            idp,
        }
    }

    /// Sign in (one pending poll, then the grant) and land on Scope.
    async fn signed_in(o: &Outlook) {
        o.m.dispatch(MailImportAction::BeginMicrosoftSignIn)
            .await
            .unwrap();
        o.m.run_microsoft_sign_in().await.unwrap();
        assert_eq!(o.m.snapshot().step, ImportStep::Scope);
    }

    fn outlook_answers(expires_in: u64) -> Vec<(u16, String)> {
        vec![
            code_answer(),
            oauth_error("authorization_pending"),
            token_answer(ACCESS, REFRESH, expires_in),
        ]
    }

    #[tokio::test]
    async fn microsoft_signin_is_unavailable_until_the_registration_and_a_transport_exist() {
        use fauna_i18n::strings::mail_import as s;
        // No transport (web): unavailable whatever the registration.
        let m = machine(&[]).with_microsoft_client(test_identity());
        assert!(!m.snapshot().microsoft_signin_available);
        // A transport, but the production constant's client ID is still empty
        // (the registration is the owner's to create).
        let idp = FakeIdentity::new([]);
        let m = machine(&[]).with_oauth_http(idp.clone());
        assert_eq!(
            m.snapshot().microsoft_signin_available,
            fauna_mail::imap_client::MICROSOFT.is_available()
        );
        m.dispatch(MailImportAction::SelectSourceKind {
            kind: ImportSourceKind::Outlook,
        })
        .await
        .unwrap();
        m.dispatch(MailImportAction::SetUsername {
            value: "person@outlook.example".into(),
        })
        .await
        .unwrap();
        if !fauna_mail::imap_client::MICROSOFT.is_available() {
            assert!(
                m.dispatch(MailImportAction::BeginMicrosoftSignIn)
                    .await
                    .is_err()
            );
            assert_eq!(
                m.snapshot().error.as_deref(),
                Some(s::MICROSOFT_UNAVAILABLE)
            );
            assert!(
                idp.requests.lock().unwrap().is_empty(),
                "never a failing request"
            );
        }
    }

    /// § Microsoft sign-in, *An admin-supplied registration*: the pair the
    /// nest reports is adopted at refresh, makes the button live even while
    /// the project's constant is unfilled, stays out of the rendered snapshot,
    /// and is the client ID and directory the device-code request carries; a
    /// cleared one falls back to the project's.
    #[tokio::test]
    async fn the_admins_registration_is_what_the_device_code_request_carries() {
        let id = "0b5c1a2e-3f4d-4e5a-8b6c-7d8e9f0a1b2c";
        let nest = Arc::new(FakeNest::new());
        *nest.registration.lock().unwrap() = Some(MicrosoftRegistration {
            client_id: id.into(),
            tenant: "contoso.example".into(),
        });
        let idp = FakeIdentity::new([code_answer()]);
        let m = MailImportMachine::new(
            nest.clone(),
            Arc::new(FakeSource::new(&[])),
            Arc::new(FakeClock::new()),
        )
        .with_oauth_http(idp.clone());

        m.dispatch(MailImportAction::Refresh).await.unwrap();
        assert!(
            m.snapshot().microsoft_signin_available,
            "an admin-set registration makes the button live"
        );
        let rendered = serde_json::to_string(&m.snapshot()).unwrap();
        assert!(
            !rendered.contains(id) && !rendered.contains("contoso.example"),
            "the registration is private state, never rendered: {rendered}"
        );

        m.dispatch(MailImportAction::SelectSourceKind {
            kind: ImportSourceKind::Outlook,
        })
        .await
        .unwrap();
        m.dispatch(MailImportAction::SetUsername {
            value: "person@contoso.example".into(),
        })
        .await
        .unwrap();
        m.dispatch(MailImportAction::BeginMicrosoftSignIn)
            .await
            .unwrap();
        let requests = idp.requests.lock().unwrap().clone();
        let (url, form) = requests.first().expect("a device-code request");
        assert!(
            form.contains(&format!("client_id={id}")),
            "the admin's client ID, not the constant's: {form}"
        );
        if std::env::var(fauna_mail::imap_client::oauth_device::E2E_AUTHORITY_ENV).is_err() {
            assert_eq!(
                url,
                "https://login.microsoftonline.com/contoso.example/oauth2/v2.0/devicecode"
            );
        }

        // Cleared on the nest: the next refresh is the project's again.
        *nest.registration.lock().unwrap() = None;
        m.dispatch(MailImportAction::Refresh).await.unwrap();
        assert_eq!(
            m.snapshot().microsoft_signin_available,
            fauna_mail::imap_client::MICROSOFT.is_available()
        );
    }

    #[tokio::test]
    async fn the_sign_in_needs_the_outlook_source_and_an_address() {
        use fauna_i18n::strings::mail_import as s;
        let o = outlook(FakeSource::new(&[]), outlook_answers(3_600)).await;
        o.m.dispatch(MailImportAction::SetUsername { value: "  ".into() })
            .await
            .unwrap();
        assert!(
            o.m.dispatch(MailImportAction::BeginMicrosoftSignIn)
                .await
                .is_err()
        );
        assert_eq!(
            o.m.snapshot().error.as_deref(),
            Some(s::MICROSOFT_NEEDS_ADDRESS)
        );
        o.m.dispatch(MailImportAction::SelectSourceKind {
            kind: ImportSourceKind::Gmail,
        })
        .await
        .unwrap();
        assert!(
            o.m.dispatch(MailImportAction::BeginMicrosoftSignIn)
                .await
                .is_err()
        );
        assert!(o.idp.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_outlook_connect_without_a_sign_in_is_refused_never_sent_a_password() {
        use fauna_i18n::strings::mail_import as s;
        let o = outlook(FakeSource::new(&[("INBOX", true, 1)]), vec![]).await;
        o.m.dispatch(MailImportAction::SetPassword {
            value: "a-typed-password".into(),
        })
        .await
        .unwrap();
        assert!(o.m.dispatch(MailImportAction::Connect).await.is_err());
        assert_eq!(
            o.m.snapshot().error.as_deref(),
            Some(s::MICROSOFT_SIGN_IN_REQUIRED)
        );
        assert!(
            o.source.auths.lock().unwrap().is_empty(),
            "nothing reached the source"
        );
    }

    #[tokio::test]
    async fn begin_shows_the_code_and_where_to_enter_it() {
        let o = outlook(FakeSource::new(&[]), outlook_answers(3_600)).await;
        assert!(o.m.snapshot().microsoft_signin_available);
        let now = o.clock.now_ms();
        o.m.dispatch(MailImportAction::BeginMicrosoftSignIn)
            .await
            .unwrap();
        let view = o.m.snapshot().microsoft_signin.expect("a sign-in view");
        assert_eq!(
            view,
            MicrosoftSignInView {
                user_code: "ABCD-EFGH".into(),
                verification_uri: "https://microsoft.com/devicelogin".into(),
                expires_at_ms: now + 900_000,
                state: MicrosoftSignInState::Waiting,
            }
        );
        let requests = o.idp.requests.lock().unwrap();
        assert_eq!(requests[0].0, "https://idp.test/devicecode");
        assert!(requests[0].1.starts_with("client_id=client-123&scope="));
    }

    #[tokio::test]
    async fn a_grant_connects_with_xoauth2_by_itself() {
        let o = outlook(
            FakeSource::new(&[("INBOX", true, 1)]),
            outlook_answers(3_600),
        )
        .await;
        signed_in(&o).await;
        assert_eq!(
            o.source.auths.lock().unwrap().as_slice(),
            &[SourceAuth::MicrosoftToken {
                user: "person@outlook.example".into(),
                access_token: ACCESS.into(),
            }]
        );
        let snap = o.m.snapshot();
        assert_eq!(snap.microsoft_signin, None);
        assert_eq!(snap.mailboxes.len(), 1);
        assert!(o.m.holds_microsoft_token());
        // One poll waited out the 5 s interval before the pending answer,
        // one more before the grant.
        assert_eq!(*o.clock.slept.lock().unwrap(), vec![5_000, 5_000]);
    }

    #[tokio::test]
    async fn a_declined_expired_or_unconsented_sign_in_says_so_and_stays_on_source() {
        use fauna_i18n::strings::mail_import as s;
        for (code, state, sentence) in [
            (
                "authorization_declined",
                MicrosoftSignInState::Declined,
                s::MICROSOFT_DECLINED.to_string(),
            ),
            (
                "expired_token",
                MicrosoftSignInState::Expired,
                s::MICROSOFT_EXPIRED.to_string(),
            ),
            (
                "consent_required",
                MicrosoftSignInState::AdminConsentRequired,
                s::microsoft_admin_consent_fmt(
                    "https://login.microsoftonline.com/organizations/adminconsent?client_id=client-123",
                ),
            ),
            (
                "bad_verification_code",
                MicrosoftSignInState::Failed,
                s::MICROSOFT_FAILED.to_string(),
            ),
        ] {
            let o = outlook(FakeSource::new(&[]), vec![code_answer(), oauth_error(code)]).await;
            o.m.dispatch(MailImportAction::BeginMicrosoftSignIn)
                .await
                .unwrap();
            assert!(o.m.run_microsoft_sign_in().await.is_err(), "{code}");
            let snap = o.m.snapshot();
            assert_eq!(snap.step, ImportStep::Source, "{code}");
            assert_eq!(snap.microsoft_signin.unwrap().state, state, "{code}");
            assert_eq!(snap.error.as_deref(), Some(sentence.as_str()), "{code}");
            assert!(!o.m.holds_microsoft_token());
            assert!(o.source.auths.lock().unwrap().is_empty(), "{code}");
        }
    }

    #[tokio::test]
    async fn a_cancelled_sign_in_ends_quietly_and_forgets_everything() {
        let o = outlook(FakeSource::new(&[]), outlook_answers(3_600)).await;
        o.m.dispatch(MailImportAction::BeginMicrosoftSignIn)
            .await
            .unwrap();
        o.m.dispatch(MailImportAction::CancelMicrosoftSignIn)
            .await
            .unwrap();
        o.m.run_microsoft_sign_in().await.unwrap();
        let snap = o.m.snapshot();
        assert_eq!(snap.microsoft_signin, None);
        assert_eq!(snap.error, None);
        assert!(!o.m.holds_microsoft_token());
        assert_eq!(
            o.idp.requests.lock().unwrap().len(),
            1,
            "no poll after cancel"
        );
    }

    #[tokio::test]
    async fn a_refused_xoauth2_connect_forgets_the_token() {
        use fauna_i18n::strings::mail_import as s;
        let o = outlook(
            FakeSource::failing_connect(SourceConnectFailure::SignIn, "NO AUTHENTICATE failed"),
            outlook_answers(3_600),
        )
        .await;
        o.m.dispatch(MailImportAction::BeginMicrosoftSignIn)
            .await
            .unwrap();
        assert!(o.m.run_microsoft_sign_in().await.is_err());
        let snap = o.m.snapshot();
        assert_eq!(
            snap.error.as_deref(),
            Some(format!("{} {}", s::CONNECT_FAILED_SIGN_IN, s::CONNECT_HINT_OUTLOOK).as_str())
        );
        assert_eq!(
            snap.microsoft_signin.unwrap().state,
            MicrosoftSignInState::Failed
        );
        assert!(!o.m.holds_microsoft_token());
    }

    /// Outcome 18's tier_1 half: through a whole Outlook import the token
    /// appears in no snapshot the UI renders and in no request the machine
    /// sends the nest, and the machine holds none once the import is Done.
    #[tokio::test]
    async fn the_token_reaches_no_snapshot_and_no_nest_request_and_ends_at_done() {
        let o = outlook(
            FakeSource::new(&[("INBOX", true, 3)]).with_messages("INBOX", 3),
            outlook_answers(3_600),
        )
        .await;
        let mut snapshots = vec![serde_json::to_string(&o.m.snapshot()).unwrap()];
        o.m.dispatch(MailImportAction::BeginMicrosoftSignIn)
            .await
            .unwrap();
        snapshots.push(serde_json::to_string(&o.m.snapshot()).unwrap());
        o.m.run_microsoft_sign_in().await.unwrap();
        snapshots.push(serde_json::to_string(&o.m.snapshot()).unwrap());
        o.m.dispatch(MailImportAction::Next).await.unwrap();
        o.m.dispatch(MailImportAction::Start).await.unwrap();
        snapshots.push(serde_json::to_string(&o.m.snapshot()).unwrap());
        o.m.run_import().await.unwrap();
        snapshots.push(serde_json::to_string(&o.m.snapshot()).unwrap());

        assert_eq!(o.nest.sent_uids.lock().unwrap().as_slice(), &[1, 2, 3]);
        let outbound = o.nest.outbound.lock().unwrap();
        assert!(outbound.iter().any(|r| r.starts_with("send_unit")));
        for secret in [ACCESS, REFRESH, "dc-SECRET"] {
            for snap in &snapshots {
                assert!(!snap.contains(secret), "{secret} in a snapshot: {snap}");
            }
            for request in outbound.iter() {
                assert!(
                    !request.contains(secret),
                    "{secret} sent to the nest: {request}"
                );
            }
        }
        assert_eq!(
            o.nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed
        );
        assert!(!o.m.holds_microsoft_token(), "zeroized at Done");
    }

    #[tokio::test]
    async fn the_token_ends_with_a_cancelled_import() {
        let o = outlook(
            FakeSource::new(&[("INBOX", true, 1)]).with_messages("INBOX", 1),
            outlook_answers(3_600),
        )
        .await;
        signed_in(&o).await;
        o.m.dispatch(MailImportAction::Next).await.unwrap();
        o.m.dispatch(MailImportAction::Start).await.unwrap();
        o.m.dispatch(MailImportAction::Cancel).await.unwrap();
        assert!(!o.m.holds_microsoft_token());
    }

    #[tokio::test]
    async fn the_token_ends_with_an_errored_import() {
        let mut source = FakeSource::new(&[("INBOX", true, 60)]).with_messages("INBOX", 60);
        for uid in 1..=60 {
            source = source.failing_uid(uid);
        }
        let o = outlook(source, outlook_answers(3_600)).await;
        signed_in(&o).await;
        o.m.dispatch(MailImportAction::Next).await.unwrap();
        o.m.dispatch(MailImportAction::Start).await.unwrap();
        assert!(o.m.run_import().await.is_err(), "the error budget breaks");
        assert!(!o.m.holds_microsoft_token());
    }

    /// § Microsoft sign-in, *Lifetime and refresh*: a reconnect after a
    /// dropped source connection refreshes a token with under five minutes
    /// left — once — and the seam signs in with the new one.
    #[tokio::test]
    async fn a_reconnect_refreshes_a_token_near_its_end_once() {
        let mut answers = outlook_answers(3_600);
        answers.push(token_answer("at-SECRET-2", "rt-SECRET-2", 3_600));
        let o = outlook(
            FakeSource::new(&[("INBOX", true, 2)])
                .with_messages("INBOX", 2)
                .queue_fetch_error("INBOX", NestError::Transient("connection reset".into())),
            answers,
        )
        .await;
        signed_in(&o).await;
        o.m.dispatch(MailImportAction::Next).await.unwrap();
        o.m.dispatch(MailImportAction::Start).await.unwrap();
        // 200 s of the hour left when the connection drops.
        o.clock.advance(3_400_000 - 10_000);
        o.m.run_import().await.unwrap();

        assert_eq!(o.idp.refreshes(), 1);
        assert_eq!(
            o.source.auths.lock().unwrap().last(),
            Some(&SourceAuth::MicrosoftToken {
                user: "person@outlook.example".into(),
                access_token: "at-SECRET-2".into(),
            })
        );
        assert_eq!(o.nest.sent_uids.lock().unwrap().as_slice(), &[1, 2]);
    }

    /// An authenticated connection outlives its token: with no reconnect
    /// there is nothing to refresh, however close the token is to its end.
    #[tokio::test]
    async fn a_live_connection_is_never_refreshed() {
        let o = outlook(
            FakeSource::new(&[("INBOX", true, 2)]).with_messages("INBOX", 2),
            outlook_answers(3_600),
        )
        .await;
        signed_in(&o).await;
        o.m.dispatch(MailImportAction::Next).await.unwrap();
        o.m.dispatch(MailImportAction::Start).await.unwrap();
        o.clock.advance(3_590_000 - 10_000);
        o.m.run_import().await.unwrap();
        assert_eq!(o.idp.refreshes(), 0);
    }

    /// A Resume is a reconnect too.
    #[tokio::test]
    async fn a_resume_refreshes_a_token_near_its_end() {
        let mut answers = outlook_answers(3_600);
        answers.push(token_answer("at-SECRET-2", "rt-SECRET-2", 3_600));
        let mut source = FakeSource::new(&[("INBOX", true, 1)]).with_messages("INBOX", 1);
        for _ in 0..4 {
            source = source.queue_examine_error("INBOX", NestError::Transient("down".into()));
        }
        let o = outlook(source, answers).await;
        signed_in(&o).await;
        o.m.dispatch(MailImportAction::Next).await.unwrap();
        o.m.dispatch(MailImportAction::Start).await.unwrap();
        o.m.run_import().await.unwrap();
        assert_eq!(
            o.m.snapshot().pause_reason,
            Some(ImportPauseReason::ConnectionLost)
        );
        assert_eq!(o.idp.refreshes(), 0, "an hour left at every retry");

        o.clock.advance(3_400_000);
        o.m.dispatch(MailImportAction::Resume).await.unwrap();
        o.m.run_import().await.unwrap();
        assert_eq!(o.idp.refreshes(), 1);
        assert_eq!(o.nest.sent_uids.lock().unwrap().as_slice(), &[1]);
    }

    /// A refused refresh ends the session `errored`, asking for the sign-in
    /// again, and forgets the token.
    #[tokio::test]
    async fn a_refused_refresh_ends_the_import_and_asks_to_sign_in_again() {
        use fauna_i18n::strings::mail_import as s;
        let mut answers = outlook_answers(3_600);
        answers.push(oauth_error("invalid_grant"));
        let o = outlook(
            FakeSource::new(&[("INBOX", true, 2)])
                .with_messages("INBOX", 2)
                .queue_fetch_error("INBOX", NestError::Transient("connection reset".into())),
            answers,
        )
        .await;
        signed_in(&o).await;
        o.m.dispatch(MailImportAction::Next).await.unwrap();
        o.m.dispatch(MailImportAction::Start).await.unwrap();
        o.clock.advance(3_400_000);
        assert!(o.m.run_import().await.is_err());
        assert_eq!(
            o.nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Errored
        );
        assert_eq!(
            o.m.snapshot().error.as_deref(),
            Some(s::MICROSOFT_SESSION_SIGN_IN_AGAIN)
        );
        assert!(!o.m.holds_microsoft_token());
    }

    // ── Resume after the app restarted (§ Resume protocol steps 1–3) ────

    /// The row a restarted app finds: a Generic source mid-walk, INBOX two
    /// messages in, Archive untouched, under a since date.
    fn a_session_left_by_an_app_that_closed(state: ImportSessionState) -> ImportSessionView {
        ImportSessionView {
            source_descriptor: "imap:person@old.example@mail.old.example".into(),
            scope: vec!["INBOX".into(), "Archive".into()],
            date_from: "2023-11-14".into(),
            imported_count: 2,
            cursors: vec![ImportMailboxCursorView {
                mailbox: "INBOX".into(),
                last_processed_source_uid: 2,
                source_uid_validity: 7,
            }],
            ..session(state, 5)
        }
    }

    /// A fresh machine — the restarted app — over a nest holding `row`.
    async fn restarted(
        row: ImportSessionView,
        source: FakeSource,
    ) -> (MailImportMachine, Arc<FakeNest>, Arc<FakeSource>) {
        let nest = Arc::new(FakeNest::new());
        *nest.session.lock().unwrap() = Some(row);
        let source = Arc::new(source);
        let m = MailImportMachine::new(nest.clone(), source.clone(), Arc::new(FakeClock::new()));
        m.hydrate().await.unwrap();
        (m, nest, source)
    }

    fn five_dated_messages() -> FakeSource {
        FakeSource::new(&[("INBOX", true, 4), ("Archive", true, 2)])
            .with_dated_messages(
                "INBOX",
                &[
                    (1, DAY_START),
                    (2, DAY_START),
                    (3, DAY_START - 86_400), // before the since date
                    (4, DAY_START),
                ],
            )
            .with_dated_messages("Archive", &[(1, DAY_START), (2, DAY_START - 86_400)])
    }

    /// Outcome 10: opening the app again lands on the same import's Progress,
    /// with the source, the scope and the since date taken from the row, and
    /// Resume waiting for the password.
    #[tokio::test]
    async fn a_restarted_app_lands_on_the_imports_progress_holding_its_source_and_scope() {
        let (m, _nest, source) = restarted(
            a_session_left_by_an_app_that_closed(ImportSessionState::Paused),
            five_dated_messages(),
        )
        .await;
        let snap = m.snapshot();
        assert_eq!(snap.step, ImportStep::Progress);
        assert_eq!(snap.session_state, Some(ImportSessionState::Paused));
        assert!(snap.resume_needs_credentials);
        assert_eq!(snap.source_kind, ImportSourceKind::Generic);
        assert_eq!(snap.username, "person@old.example");
        assert_eq!(snap.host, "mail.old.example");
        assert_eq!(snap.date_from, "2023-11-14");
        let scope: Vec<(&str, bool)> = snap
            .mailboxes
            .iter()
            .map(|m| (m.name.as_str(), m.selected))
            .collect();
        assert_eq!(scope, [("INBOX", true), ("Archive", true)]);
        assert!(
            source.connects.lock().unwrap().is_empty(),
            "nothing dialled yet"
        );
    }

    /// Outcome 28: a Resume without the password is refused with a sentence
    /// and reaches neither the source nor the nest.
    #[tokio::test]
    async fn a_resume_after_a_restart_without_the_password_reaches_nothing() {
        use fauna_i18n::strings::mail_import as s;
        let (m, nest, source) = restarted(
            a_session_left_by_an_app_that_closed(ImportSessionState::Paused),
            five_dated_messages(),
        )
        .await;
        assert!(m.dispatch(MailImportAction::Resume).await.is_err());
        let snap = m.snapshot();
        assert_eq!(snap.error.as_deref(), Some(s::RESUME_NEEDS_PASSWORD));
        assert!(snap.resume_needs_credentials);
        assert!(source.connects.lock().unwrap().is_empty());
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Paused
        );
        assert!(
            !nest
                .outbound
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.starts_with("resume_session")),
            "the nest was asked to resume"
        );
    }

    /// Outcomes 10, 26 and 28 together: with the password the Resume opens
    /// the source the row names, the walk continues after each mailbox's
    /// recorded cursor, and the since date still keeps older mail out.
    #[tokio::test]
    async fn a_resume_after_a_restart_continues_after_the_cursor_under_the_since_date() {
        let (m, nest, source) = restarted(
            a_session_left_by_an_app_that_closed(ImportSessionState::Paused),
            five_dated_messages(),
        )
        .await;
        m.dispatch(MailImportAction::SetPassword {
            value: "the-password".into(),
        })
        .await
        .unwrap();
        m.dispatch(MailImportAction::Resume).await.unwrap();
        assert_eq!(
            source.connects.lock().unwrap().as_slice(),
            &[(
                ImportSourceKind::Generic,
                "mail.old.example".to_string(),
                993,
                ImportTlsMode::Implicit,
                "person@old.example".to_string()
            )]
        );
        assert_eq!(
            source.auths.lock().unwrap().as_slice(),
            &[SourceAuth::Password("the-password".into())]
        );
        let snap = m.snapshot();
        assert!(!snap.resume_needs_credentials);
        assert!(
            snap.password.as_str().is_empty(),
            "the password leaves the snapshot once it is used"
        );
        assert_eq!(snap.session_state, Some(ImportSessionState::Running));

        m.run_import().await.unwrap();
        // INBOX resumes after uid 2 and its uid 3 is out of range; Archive
        // walks from its first message and leaves its old one out.
        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[4, 1]);
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed
        );
    }

    /// A row its app left `running` (closed or crashed without pausing): the
    /// nest refuses a resume of a running session, so Resume reconnects and
    /// the run starts here without asking the nest to resume it.
    #[tokio::test]
    async fn a_running_row_a_closed_app_left_behind_resumes_without_a_nest_resume() {
        let (m, nest, _source) = restarted(
            a_session_left_by_an_app_that_closed(ImportSessionState::Running),
            five_dated_messages(),
        )
        .await;
        assert_eq!(m.snapshot().step, ImportStep::Progress);
        m.dispatch(MailImportAction::SetPassword {
            value: "the-password".into(),
        })
        .await
        .unwrap();
        m.dispatch(MailImportAction::Resume).await.unwrap();
        assert!(
            !nest
                .outbound
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.starts_with("resume_session")),
            "a running session was asked to resume"
        );
        m.run_import().await.unwrap();
        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[4, 1]);
    }

    /// A full mailbox paused the import at INBOX uid 2: the nest keeps the
    /// row's cursor just short of the message it turned away
    /// (`mailbox-migration.md` § Failure handling; the nest's
    /// `a_quota_refusal_holds_the_rows_cursor_until_the_message_lands`). Once
    /// room is freed, the app opened again resumes from the row and offers
    /// that message first, then the rest.
    #[tokio::test]
    async fn a_resume_after_a_restart_offers_again_what_a_full_mailbox_turned_away() {
        let row = ImportSessionView {
            imported_count: 1,
            skipped_count: 1,
            cursors: vec![ImportMailboxCursorView {
                mailbox: "INBOX".into(),
                last_processed_source_uid: 1,
                source_uid_validity: 7,
            }],
            ..a_session_left_by_an_app_that_closed(ImportSessionState::Paused)
        };
        let source = FakeSource::new(&[("INBOX", true, 4), ("Archive", true, 1)])
            .with_dated_messages(
                "INBOX",
                &[
                    (1, DAY_START),
                    (2, DAY_START),
                    (3, DAY_START),
                    (4, DAY_START),
                ],
            )
            .with_dated_messages("Archive", &[(1, DAY_START)]);
        let (m, nest, _source) = restarted(row, source).await;
        m.dispatch(MailImportAction::SetPassword {
            value: "the-password".into(),
        })
        .await
        .unwrap();
        m.dispatch(MailImportAction::Resume).await.unwrap();
        m.run_import().await.unwrap();
        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[2, 3, 4, 1]);
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed
        );
    }

    /// § Opt-out per session: "Import duplicates anyway" starts off, so a
    /// message the nest already holds is skipped as a duplicate; turned on at
    /// the Scope step it is recorded on the session row at Start and every
    /// message is sent with `skip_dedup`, so the same message imports again.
    #[tokio::test]
    async fn the_duplicates_option_is_off_by_default_and_imports_again_when_on() {
        assert!(!MailImportSnapshot::empty().import_duplicates);

        let nest = Arc::new(FakeNest::new().with_dedup_uids([2]));
        let source = FakeSource::new(&[("INBOX", true, 3)]).with_messages("INBOX", 3);
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;
        m.run_import().await.unwrap();
        let snap = m.snapshot();
        assert_eq!((snap.imported_count, snap.skipped_count), (2, 1));
        assert!(!nest.session.lock().unwrap().as_ref().unwrap().skip_dedup);

        let nest = Arc::new(FakeNest::new().with_dedup_uids([2]));
        let source = FakeSource::new(&[("INBOX", true, 3)]).with_messages("INBOX", 3);
        let m = MailImportMachine::new(nest.clone(), Arc::new(source), Arc::new(FakeClock::new()));
        connected(&m).await;
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::SetImportDuplicates { value: true })
            .await
            .unwrap();
        m.dispatch(MailImportAction::Next).await.unwrap();
        m.dispatch(MailImportAction::Start).await.unwrap();
        assert!(
            nest.session.lock().unwrap().as_ref().unwrap().skip_dedup,
            "the choice is recorded on the row at Start"
        );
        m.run_import().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(
            (snap.imported_count, snap.skipped_count),
            (3, 0),
            "{snap:?}"
        );
        assert!(snap.error_log.is_empty(), "{:?}", snap.error_log);
    }

    /// § Resume protocol step 3: a wizard that finds a session it did not
    /// start takes the duplicates choice off the row, so a resume after a
    /// restart sends what the user chose — neither duplicating mail they never
    /// asked to, nor quietly dropping the snapshot they did.
    #[tokio::test]
    async fn a_resume_after_a_restart_keeps_the_sessions_duplicates_choice() {
        for chosen in [true, false] {
            let row = ImportSessionView {
                skip_dedup: chosen,
                ..a_session_left_by_an_app_that_closed(ImportSessionState::Paused)
            };
            let nest = Arc::new(FakeNest::new().with_dedup_uids([3]));
            *nest.session.lock().unwrap() = Some(row);
            let source = FakeSource::new(&[("INBOX", true, 3), ("Archive", true, 0)])
                .with_dated_messages("INBOX", &[(1, DAY_START), (2, DAY_START), (3, DAY_START)]);
            let m =
                MailImportMachine::new(nest.clone(), Arc::new(source), Arc::new(FakeClock::new()));
            m.hydrate().await.unwrap();
            assert_eq!(m.snapshot().import_duplicates, chosen);
            m.dispatch(MailImportAction::SetPassword {
                value: "the-password".into(),
            })
            .await
            .unwrap();
            m.dispatch(MailImportAction::Resume).await.unwrap();
            m.run_import().await.unwrap();
            let skipped = m.snapshot().error_log.iter().any(|l| l.ends_with("dedup"));
            assert_eq!(skipped, !chosen, "chosen = {chosen}: {:?}", m.snapshot());
        }
    }

    /// A run started on an adopted session before its Resume reached the
    /// source — an app spawning `run_import` on the `running` row a closed
    /// app left behind, after a Resume the machine refused — must not walk:
    /// every mailbox would answer "not connected" and the session would be
    /// finalized with nothing read. It refuses, and the row is left as it was.
    #[tokio::test]
    async fn a_run_on_an_adopted_session_waits_for_the_resume_to_reach_the_source() {
        use fauna_i18n::strings::mail_import as s;
        let (m, nest, source) = restarted(
            a_session_left_by_an_app_that_closed(ImportSessionState::Running),
            five_dated_messages(),
        )
        .await;
        assert!(m.dispatch(MailImportAction::Resume).await.is_err());
        assert!(m.run_import().await.is_err());
        assert_eq!(
            m.snapshot().error.as_deref(),
            Some(s::RESUME_NEEDS_PASSWORD)
        );
        assert!(nest.sent_uids.lock().unwrap().is_empty());
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Running,
            "the row was moved"
        );
        assert!(source.connects.lock().unwrap().is_empty());
    }

    /// A refused password keeps the person on Progress with the reason and
    /// the field still filled for a retry (§ Credential handling), and
    /// resumes nothing.
    #[tokio::test]
    async fn a_refused_password_on_a_resume_after_a_restart_says_so_and_resumes_nothing() {
        let (m, nest, _source) = restarted(
            a_session_left_by_an_app_that_closed(ImportSessionState::Paused),
            FakeSource::rejecting(),
        )
        .await;
        m.dispatch(MailImportAction::SetPassword {
            value: "a-wrong-password".into(),
        })
        .await
        .unwrap();
        assert!(m.dispatch(MailImportAction::Resume).await.is_err());
        // The app spawns a run on the `running`-looking row anyway; it refuses
        // without overwriting why the Resume failed.
        assert!(m.run_import().await.is_err());
        let snap = m.snapshot();
        assert_eq!(snap.step, ImportStep::Progress);
        assert!(snap.resume_needs_credentials);
        assert_eq!(snap.password.as_str(), "a-wrong-password");
        assert_eq!(
            snap.error,
            Some(connect_failure_sentence(
                ImportSourceKind::Generic,
                SourceConnectFailure::SignIn
            ))
        );
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Paused
        );
    }

    /// The same app returning to its own session (a page revisit, a stale
    /// nudge) is not a restart: it keeps its credential and its cursors.
    #[tokio::test]
    async fn the_app_that_started_the_import_does_not_ask_for_the_password_again() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3)]).with_messages("INBOX", 3);
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;
        m.dispatch(MailImportAction::Pause).await.unwrap();
        m.hydrate().await.unwrap();
        assert!(!m.snapshot().resume_needs_credentials);
        m.dispatch(MailImportAction::Resume).await.unwrap();
        m.run_import().await.unwrap();
        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 2, 3]);
    }

    /// The Outlook source asks for the Microsoft sign-in instead, and the
    /// grant waits for the person's Resume rather than leaving Progress.
    #[tokio::test]
    async fn an_outlook_resume_after_a_restart_signs_in_with_microsoft_again() {
        use fauna_i18n::strings::mail_import as s;
        let o = outlook(
            FakeSource::new(&[("INBOX", true, 1)]).with_messages("INBOX", 1),
            outlook_answers(3_600),
        )
        .await;
        *o.nest.session.lock().unwrap() = Some(ImportSessionView {
            source_descriptor: "outlook:person@outlook.example@outlook.office365.com".into(),
            scope: vec!["INBOX".into()],
            ..session(ImportSessionState::Paused, 1)
        });
        o.m.hydrate().await.unwrap();
        assert!(o.m.snapshot().resume_needs_credentials);
        assert!(o.m.dispatch(MailImportAction::Resume).await.is_err());
        assert_eq!(
            o.m.snapshot().error.as_deref(),
            Some(s::RESUME_NEEDS_MICROSOFT)
        );

        o.m.dispatch(MailImportAction::BeginMicrosoftSignIn)
            .await
            .unwrap();
        o.m.run_microsoft_sign_in().await.unwrap();
        let snap = o.m.snapshot();
        assert_eq!(snap.step, ImportStep::Progress, "the grant left Progress");
        assert!(o.source.connects.lock().unwrap().is_empty());

        o.m.dispatch(MailImportAction::Resume).await.unwrap();
        assert_eq!(
            o.source.auths.lock().unwrap().as_slice(),
            &[SourceAuth::MicrosoftToken {
                user: "person@outlook.example".into(),
                access_token: ACCESS.into(),
            }]
        );
        o.m.run_import().await.unwrap();
        assert_eq!(o.nest.sent_uids.lock().unwrap().as_slice(), &[1]);
    }

    #[test]
    fn resume_actions_commit_the_credential_only_for_an_adopted_session() {
        let pw = || SecretString::from("pw".to_string());
        assert_eq!(
            resume_actions(false, ImportSourceKind::Generic, "143", pw()),
            vec![MailImportAction::Resume]
        );
        assert_eq!(
            resume_actions(true, ImportSourceKind::Generic, " 143 ", pw()),
            vec![
                MailImportAction::SetPort { value: 143 },
                MailImportAction::SetPassword { value: pw() },
                MailImportAction::Resume,
            ]
        );
        assert_eq!(
            resume_actions(true, ImportSourceKind::Gmail, "143", pw()),
            vec![
                MailImportAction::SetPassword { value: pw() },
                MailImportAction::Resume,
            ],
            "a provider's port is its own"
        );
        assert_eq!(
            resume_actions(true, ImportSourceKind::Generic, "not a port", pw()),
            vec![
                MailImportAction::SetPassword { value: pw() },
                MailImportAction::Resume,
            ]
        );
    }

    #[test]
    fn a_source_descriptor_reads_back_to_its_provider_user_and_host() {
        assert_eq!(
            parse_source_descriptor("imap:person@old.example@mail.old.example"),
            Some((
                ImportSourceKind::Generic,
                "person@old.example".into(),
                "mail.old.example".into()
            ))
        );
        let (descriptor, ..) = {
            let mut snap = MailImportSnapshot::empty();
            snap.source_kind = ImportSourceKind::ICloud;
            snap.username = "me@icloud.example".into();
            snap.host = "imap.mail.me.com".into();
            snap.start_args()
        };
        assert_eq!(
            parse_source_descriptor(&descriptor),
            Some((
                ImportSourceKind::ICloud,
                "me@icloud.example".into(),
                "imap.mail.me.com".into()
            ))
        );
        assert_eq!(parse_source_descriptor(""), None);
        assert_eq!(parse_source_descriptor("pop3:a@b"), None);
    }

    // ── The remembered source (§ Credential handling, outcome 29) ───────

    /// The account's record, in memory: what every machine sharing it reads.
    #[derive(Default)]
    struct FakeSourceMemory {
        record: StdMutex<Option<RememberedImportSource>>,
        saves: StdMutex<usize>,
    }

    #[async_trait]
    impl MailImportSourceStore for FakeSourceMemory {
        async fn load(&self) -> Result<Option<RememberedImportSource>, crate::error::StoreError> {
            Ok(self.record.lock().unwrap().clone())
        }

        async fn save(
            &self,
            next: Option<RememberedImportSource>,
        ) -> Result<(), crate::error::StoreError> {
            *self.saves.lock().unwrap() += 1;
            *self.record.lock().unwrap() = next;
            Ok(())
        }
    }

    fn remembering(memory: &Arc<FakeSourceMemory>, source: FakeSource) -> MailImportMachine {
        MailImportMachine::new(
            Arc::new(FakeNest::new()),
            Arc::new(source),
            Arc::new(FakeClock::new()),
        )
        .with_source_memory(memory.clone())
    }

    async fn connect_generic(m: &MailImportMachine) {
        for action in [
            MailImportAction::SelectSourceKind {
                kind: ImportSourceKind::Generic,
            },
            MailImportAction::SetHost {
                value: "mail.old.example".into(),
            },
            MailImportAction::SetPort { value: 143 },
            MailImportAction::SetTlsMode {
                mode: ImportTlsMode::StartTls,
            },
            MailImportAction::SetUsername {
                value: "person@old.example".into(),
            },
            MailImportAction::SetPassword {
                value: "secret".into(),
            },
            MailImportAction::Connect,
        ] {
            m.dispatch(action).await.unwrap();
        }
    }

    /// Outcome 29: a successful connection remembers the server, port,
    /// connection security and username — never the password — and the next
    /// visit (a fresh machine over the same account) finds them filled in.
    #[tokio::test]
    async fn a_connected_source_is_remembered_and_prefills_the_next_visit() {
        let memory = Arc::new(FakeSourceMemory::default());
        let first = remembering(&memory, FakeSource::new(&[("INBOX", true, 1)]));
        first.hydrate().await.unwrap();
        assert!(!first.snapshot().source_remembered);
        connect_generic(&first).await;
        assert!(first.snapshot().source_remembered);
        assert_eq!(
            memory.record.lock().unwrap().clone(),
            Some(RememberedImportSource {
                provider: "imap".into(),
                host: "mail.old.example".into(),
                port: 143,
                tls_mode: "starttls".into(),
                username: "person@old.example".into(),
            })
        );

        let next = remembering(&memory, FakeSource::new(&[("INBOX", true, 1)]));
        next.hydrate().await.unwrap();
        let snap = next.snapshot();
        assert_eq!(snap.step, ImportStep::Source);
        assert!(snap.source_remembered);
        assert_eq!(snap.source_kind, ImportSourceKind::Generic);
        assert_eq!(snap.host, "mail.old.example");
        assert_eq!(snap.port, 143);
        assert_eq!(snap.tls_mode, ImportTlsMode::StartTls);
        assert_eq!(snap.username, "person@old.example");
        assert!(snap.password.as_str().is_empty());
    }

    /// A failed connection remembers nothing, and a refresh never writes over
    /// a username the user has started typing.
    #[tokio::test]
    async fn a_refused_connection_remembers_nothing_and_typing_is_never_overwritten() {
        let memory = Arc::new(FakeSourceMemory::default());
        let mut source = FakeSource::new(&[("INBOX", true, 1)]);
        source.reject = true;
        let m = remembering(&memory, source);
        assert!(
            m.dispatch(MailImportAction::SetUsername {
                value: "typed@example.com".into(),
            })
            .await
            .is_ok()
        );
        m.dispatch(MailImportAction::SetPassword { value: "x".into() })
            .await
            .unwrap();
        assert!(m.dispatch(MailImportAction::Connect).await.is_err());
        assert_eq!(*memory.saves.lock().unwrap(), 0);

        *memory.record.lock().unwrap() = Some(RememberedImportSource {
            provider: "gmail".into(),
            host: "imap.gmail.com".into(),
            port: 993,
            tls_mode: "implicit".into(),
            username: "remembered@example.com".into(),
        });
        m.dispatch(MailImportAction::Refresh).await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.username, "typed@example.com");
        assert!(snap.source_remembered);
    }

    /// Outcome 29's "until you delete them": Forget deletes the account's
    /// record and leaves a first visit's fields; the next visit is blank.
    #[tokio::test]
    async fn forgetting_the_source_deletes_it_and_the_next_visit_is_blank() {
        let memory = Arc::new(FakeSourceMemory::default());
        let first = remembering(&memory, FakeSource::new(&[("INBOX", true, 1)]));
        connect_generic(&first).await;

        let next = remembering(&memory, FakeSource::new(&[("INBOX", true, 1)]));
        next.hydrate().await.unwrap();
        assert_eq!(next.snapshot().username, "person@old.example");
        next.dispatch(MailImportAction::ForgetSource).await.unwrap();
        let snap = next.snapshot();
        assert!(!snap.source_remembered);
        assert_eq!(memory.record.lock().unwrap().clone(), None);
        let empty = MailImportSnapshot::empty();
        assert_eq!(
            (
                snap.source_kind,
                snap.host.as_str(),
                snap.port,
                snap.username.as_str()
            ),
            (empty.source_kind, empty.host.as_str(), empty.port, "")
        );

        let last = remembering(&memory, FakeSource::new(&[("INBOX", true, 1)]));
        last.hydrate().await.unwrap();
        assert!(!last.snapshot().source_remembered);
        assert_eq!(last.snapshot().username, "");
    }

    /// A machine with no store to forget from refuses Forget, and never
    /// claims a remembered source.
    #[tokio::test]
    async fn without_a_store_nothing_is_remembered_and_forget_is_refused() {
        let m = machine(&[("INBOX", true, 1)]);
        connected(&m).await;
        assert!(!m.snapshot().source_remembered);
        assert!(m.dispatch(MailImportAction::ForgetSource).await.is_err());
    }

    /// § Resume protocol step 2: an app adopting a Generic session after a
    /// restart takes the port and connection security the account remembers
    /// for that host and username, and the 993 implicit-TLS default for any
    /// other — then remembers what the person corrected once the source
    /// answers.
    #[tokio::test]
    async fn a_resume_after_a_restart_reuses_the_remembered_port_and_security() {
        let remembered = |host: &str| RememberedImportSource {
            provider: "imap".into(),
            host: host.into(),
            port: 143,
            tls_mode: "starttls".into(),
            username: "person@old.example".into(),
        };
        for (host, port, tls) in [
            ("mail.old.example", 143, ImportTlsMode::StartTls),
            ("another.example", 993, ImportTlsMode::Implicit),
        ] {
            let memory = Arc::new(FakeSourceMemory::default());
            *memory.record.lock().unwrap() = Some(remembered(host));
            let nest = Arc::new(FakeNest::new());
            *nest.session.lock().unwrap() = Some(a_session_left_by_an_app_that_closed(
                ImportSessionState::Paused,
            ));
            let source = Arc::new(five_dated_messages());
            let m = MailImportMachine::new(nest, source.clone(), Arc::new(FakeClock::new()))
                .with_source_memory(memory.clone());
            m.hydrate().await.unwrap();
            let snap = m.snapshot();
            assert_eq!(snap.step, ImportStep::Progress);
            assert_eq!(snap.host, "mail.old.example");
            assert_eq!((snap.port, snap.tls_mode), (port, tls), "remembered {host}");

            m.dispatch(MailImportAction::SetPort { value: 10143 })
                .await
                .unwrap();
            m.dispatch(MailImportAction::SetPassword {
                value: "secret".into(),
            })
            .await
            .unwrap();
            m.dispatch(MailImportAction::Resume).await.unwrap();
            assert_eq!(source.connects.lock().unwrap()[0].2, 10143);
            let record = memory.record.lock().unwrap().clone().unwrap();
            assert_eq!(
                (record.host.as_str(), record.port),
                ("mail.old.example", 10143)
            );
        }
    }

    /// A record this build cannot read prefills nothing rather than a guess.
    #[test]
    fn an_unreadable_remembered_source_prefills_nothing() {
        let mut snap = MailImportSnapshot::empty();
        for bad in [
            RememberedImportSource {
                provider: "pop3".into(),
                host: "h".into(),
                port: 1,
                tls_mode: "implicit".into(),
                username: "u".into(),
            },
            RememberedImportSource {
                provider: "imap".into(),
                host: "h".into(),
                port: 1,
                tls_mode: "plaintext".into(),
                username: "u".into(),
            },
            RememberedImportSource::default(),
        ] {
            snap.prefill_from(&bad);
            assert_eq!(snap, MailImportSnapshot::empty());
        }
    }
}
