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
//! Per-message error budget) or an exhausted source retry (§ Failure
//! handling's 5 s / 30 s / 2 min TCP/TLS table) calls
//! [`MailImportNest::fail_session`] and stops.
//!
//! **Same-process resume only.** The per-mailbox [`MailboxCursor`]s this loop
//! tracks live in [`MailImportMachine`]'s own memory, not on the wire — a
//! `Pause` → `Resume` within one running app instance continues from exactly
//! where it left off, but a real client restart (§ Resume protocol step 2,
//! "the user re-enters the password") starts every selected mailbox over from
//! its first UID. This is never *incorrect* — nest's dedup path skips
//! messages it has already imported — only less efficient than the doc's full
//! restart-resume, which needs the nest-side `scope`/cursor fields this
//! machine does not yet read back from [`ImportSessionView`] (they exist on
//! the wire — `ImportSessionInfo.cursors` — but wiring a real restart-resume
//! is real design surface of its own, left for a follow-on pass).
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
//! Also deliberately out of scope: the Outlook OAuth flow (§ Wizard steps
//! step 1's Microsoft Graph path mirrors the Bluesky bridge's client-side
//! OAuth pattern, `fauna-client-bridges/src/atproto.rs` — a distinct, well
//! scoped follow-up). Outlook accounts use the IMAP user/password fallback
//! path this module already supports; `mail-import-source-oauth-button` has
//! no wired action yet. And the restart-survivable resume noted above.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_core::localized::LocalizedText;
use fauna_core::secret::SecretString;
use fauna_mail::imap_client::{BatchPacker, FetchOutcome, ImportUnit, MailboxStatus};
use fauna_protocol::MaybeSendSync;
use serde::{Deserialize, Serialize};

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
    /// The source's reported message count (`EXAMINE`'s `EXISTS`), for the
    /// Confirm step's total estimate. `0` when the real seam has not yet
    /// populated it — an estimate the doc itself says gets "revised as the
    /// client enumerates the source" (`ImportSessionInfo.total_count`).
    pub message_count: u32,
}

/// One source mailbox as `LIST` reports it, before selection state exists.
/// What [`ImportSourceNest::list_source_mailboxes`] returns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceMailboxView {
    pub name: String,
    /// `false` for a `\Noselect` container (e.g. Gmail's bare `[Gmail]`).
    pub selectable: bool,
    pub message_count: u32,
}

/// [`run_import`](MailImportMachine::run_import)'s in-memory resume position
/// for one mailbox this run (§ Resume protocol) — the same-process-only
/// analogue of the wire's `ImportMailboxCursor`. `None` (no cursor yet) is
/// what a mailbox never yet examined this run passes to
/// [`ImportSourceNest::examine`] / [`ImportSourceNest::fetch_window`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxCursor {
    pub last_processed_source_uid: u32,
    pub source_uid_validity: u32,
}

/// A live/finished import session as the nest seam reports it (projects
/// `ImportSessionInfo`, dropping the per-mailbox resume cursors — the wizard
/// UI never renders them; a future `run_import` keeps them internally).
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

    fn is_bounded(&self) -> bool {
        self.since.is_some()
    }

    fn admits(&self, internal_date: i64) -> bool {
        self.since.is_none_or(|s| internal_date >= s)
    }
}

/// Coarse machine status for spinner / disabled-control rendering.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ImportStatus {
    Idle,
    Loading,
    Working,
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
    // ── progress / done (steps 5–6; from the session) ──────────────
    pub session_state: Option<ImportSessionState>,
    pub imported_count: u64,
    pub skipped_count: u64,
    pub errored_count: u64,
    pub total_count: u64,
    /// Per-message skip/error lines (`mail-import-error-log`). Populated by a
    /// future `run_import`; empty in this slice.
    pub error_log: Vec<String>,
    pub status: ImportStatus,
    /// Last action's error, surfaced via `error-message`.
    pub error: Option<String>,
}

/// § Wizard steps step 3's default mailbox selection, generalized from
/// `mail-export.md`'s `Trash`/`Junk` rule: skip mailboxes that would
/// duplicate content already covered by other selected mailboxes
/// (`Spam`/`Trash`/`Junk`, and Gmail's own `[Gmail]/All Mail` +
/// `[Gmail]/Bin` labels by exact name).
fn default_selected(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !matches!(
        lower.as_str(),
        "trash" | "junk" | "spam" | "[gmail]/all mail" | "[gmail]/bin"
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
            session_state: None,
            imported_count: 0,
            skipped_count: 0,
            errored_count: 0,
            total_count: 0,
            error_log: Vec::new(),
            status: ImportStatus::Idle,
            error: None,
        }
    }

    /// The `(source_descriptor, total_count, scope, date_from)` tuple
    /// `start_session` sends — descriptor never carries the password
    /// (`mailbox-migration.md` § Progress lives nest-side:
    /// `source_descriptor` is "provider + hostname + username — no
    /// password"); `scope` is every selected mailbox's name, recorded so a
    /// resumed session knows what to re-`EXAMINE` (§ Resume protocol); and
    /// `date_from` is the scope step's since date, recorded for the same
    /// reason one step on — the row is the only durable record of how much
    /// of those mailboxes the session was allowed to take.
    fn start_args(&self) -> (String, u64, Vec<String>, String) {
        let descriptor = format!(
            "{}:{}@{}",
            source_kind_label(self.source_kind),
            self.username,
            self.host
        );
        let selected: Vec<&SourceMailboxOption> =
            self.mailboxes.iter().filter(|m| m.selected).collect();
        let total = selected.iter().map(|m| u64::from(m.message_count)).sum();
        let scope = selected.into_iter().map(|m| m.name.clone()).collect();
        (descriptor, total, scope, self.date_from.clone())
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
        self.session_state = Some(s.state);
        self.imported_count = s.imported_count;
        self.skipped_count = s.skipped_count;
        self.errored_count = s.errored_count;
        self.total_count = s.total_count;
        if s.state == ImportSessionState::Errored && !s.error_reason.is_empty() {
            self.error = Some(s.error_reason);
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

/// Actions the per-app UI dispatches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum MailImportAction {
    /// Page load / resume: jump to a resumable (`running` / `paused`) session's
    /// Progress screen, if one exists. A `completed` session is not resumed.
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
/// **Precondition for `Gmail`/`ICloud`: the caller must already have
/// dispatched `SelectSourceKind { kind }` to the same machine for the picked
/// kind.** Omitting `SetHost`/`SetPort` here is safe only because
/// `apply_client_action`'s `SelectSourceKind` arm is what wrote
/// `host`/`port`/`tls_mode` via `provider_preset` — this function does not
/// derive the connect-time host from `kind` itself, it trusts the snapshot
/// already carries it. A caller that keeps the picked kind only in local UI
/// state and never dispatches `SelectSourceKind` sends a stale/attacker host
/// with a freshly-typed provider password (mailbox-migration.md § Wizard
/// steps).
///
/// **`Outlook` and `Generic` are IMAP-fallback kinds and never hit that
/// precondition** — both always send whatever `host`/`port` the caller
/// passes *here*, ignoring the snapshot entirely, because each sits beside a
/// disabled OAuth button and needs a field the user can actually type into.
/// `Outlook`'s `provider_preset` host (`outlook.office365.com`) is therefore
/// **advisory-only**: `SelectSourceKind` does write it into the snapshot the
/// same way it does for `Gmail`/`ICloud`, but this function never reads that
/// copy back for `Outlook` — the value dispatched is always the caller's own,
/// painted as a default a user can overtype, never re-derived from `kind`.
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
    if matches!(kind, ImportSourceKind::Outlook | ImportSourceKind::Generic) {
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
    /// `fauna.bridges.start_import_session`. `scope` is every mailbox the
    /// wizard's scope step selected (§ Resume protocol); `date_from` is the
    /// scope step's since date, empty for unbounded (§ Wizard steps step 3).
    async fn start_session(
        &self,
        source_descriptor: String,
        total_count: u64,
        scope: Vec<String>,
        date_from: String,
    ) -> Result<ImportSessionView, NestError>;
    /// `fauna.bridges.pause_import_session`.
    async fn pause_session(&self, session_id: String) -> Result<ImportSessionView, NestError>;
    /// `fauna.bridges.resume_import_session`.
    async fn resume_session(&self, session_id: String) -> Result<ImportSessionView, NestError>;
    /// `fauna.bridges.cancel_import_session` — already-imported messages are
    /// kept.
    async fn cancel_session(&self, session_id: String) -> Result<ImportSessionView, NestError>;
    /// `fauna.bridges.finalize_import_session`. Not yet dispatched by this
    /// module (no `MailImportAction` variant calls it) — the future
    /// `run_import` calls it at end-of-enumeration.
    async fn finalize_session(&self, session_id: String) -> Result<ImportSessionView, NestError>;
    /// `fauna.bridges.fail_import_session`. Called by
    /// [`MailImportMachine::run_import`] on a session-fatal source error or a
    /// § Per-message error budget breach.
    async fn fail_session(
        &self,
        session_id: String,
        reason: String,
    ) -> Result<ImportSessionView, NestError>;

    /// `fauna.bridges.import_message` / `_message_batch`, dispatched by
    /// `unit`'s variant (§ Batching). `skip_dedup` mirrors the wizard's dedup
    /// opt-out (§ Dedup § Opt-out per session — not yet surfaced in ui.yaml
    /// this pass; `run_import` always sends `false` today). The real
    /// `rpc_glue` impl wraps `fauna_mail::imap_client::MailImportClient::send_unit`,
    /// projecting its wire reply's `ImportMessageOutcome`(s) down to
    /// [`SendOutcome`].
    /// `revised_total_count` replaces the session's stored estimate when the
    /// loop has learned something truer than the source's `EXISTS` — which a
    /// since date always makes it, since `EXISTS` counts a whole mailbox and
    /// the range takes only part of it (§ Progress lives nest-side's
    /// `total_count`). `None` leaves the stored total alone.
    async fn send_unit(
        &self,
        session_id: String,
        unit: ImportUnit,
        skip_dedup: bool,
        revised_total_count: Option<u64>,
    ) -> Result<SendUnitReply, NestError>;
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
    pub password: SecretString,
}

/// § Failure handling's TCP/TLS retry backoff (5 s / 30 s / 2 min) as an
/// injectable seam — `run_import`'s tests prove the *requested* delays
/// (`testing.md` convention 14: never a real wall-clock wait), and the real
/// impl is `fauna_sleep::sleep`. Mirrors
/// `fauna_mail::imap_client::ImapClock`'s `sleep_ms` shape, but as its own
/// dyn-safe `async_trait` (unlike `ImapClock`'s AFIT) since
/// [`MailImportMachine`] holds every seam as `Arc<dyn _>`.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait RetryClock: MaybeSendSync {
    /// Sleep for at least `ms` milliseconds.
    async fn sleep_ms(&self, ms: u64);
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
    /// `LOGIN` (or `AUTHENTICATE XOAUTH2`, once OAuth lands) against the
    /// source. Refuses a plaintext-downgrade exactly as `ImapSession` does;
    /// the client memory holding `params.password` is the caller's, dropped
    /// after this call per § Credential handling.
    async fn connect(&self, params: SourceConnectParams) -> Result<(), NestError>;
    /// `LIST "" "*"` — every mailbox the account can see, `\Noselect`
    /// containers included (this module filters them, § `SourceMailboxOption`
    /// docs), with each mailbox's message count for the Confirm estimate.
    async fn list_source_mailboxes(&self) -> Result<Vec<SourceMailboxView>, NestError>;

    /// `EXAMINE` `mailbox` read-only (never `SELECT` — § per-message flow
    /// step 1's precedent: the source must be left exactly as the user found
    /// it). When `cursor` is `Some`, validates its `source_uid_validity`
    /// against what the source reports now (§ Resume protocol step 4); a
    /// mismatch is [`NestError::Rejected`] — per-mailbox, not session-fatal,
    /// the caller logs it and moves to the next mailbox (this pass has no
    /// restart-import UI — no ui.yaml surface for it yet).
    async fn examine(
        &self,
        mailbox: &str,
        cursor: Option<MailboxCursor>,
    ) -> Result<MailboxStatus, NestError>;

    /// One throttled fetch window (§ Throttling — the real seam owns the
    /// per-source-server rate cap internally): at most `max` messages
    /// starting after `cursor`'s `last_processed_source_uid` (from the
    /// mailbox's first message when `cursor` is `None`). An empty result
    /// means the mailbox is exhausted. [`NestError::Transient`] for a
    /// retryable TCP/TLS fault (§ Failure handling's 3-retry backoff);
    /// [`NestError::Rejected`] for a session-fatal source error (e.g. the
    /// source auth expired mid-run).
    async fn fetch_window(
        &self,
        mailbox: &str,
        cursor: Option<MailboxCursor>,
        max: usize,
    ) -> Result<Vec<FetchOutcome>, NestError>;

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
}

/// § Failure handling: "Up to 3 retries with exponential backoff (5 s, 30 s,
/// 2 min) before moving the session to errored."
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
    /// `run_import`'s per-mailbox resume position, same-process-only (module
    /// docs). Reset on a fresh `Start`; survives `Pause`/`Resume`.
    cursors: Mutex<HashMap<String, MailboxCursor>>,
    /// § Per-message error budget's rolling window. Same lifetime as
    /// `cursors` — one budget per import run.
    error_budget: Mutex<ErrorBudget>,
    stop: Mutex<StopRequest>,
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
        }
    }

    fn set_status(&self, status: ImportStatus) {
        self.inner.lock().expect("snapshot mutex").status = status;
    }

    fn store_session(&self, session: ImportSessionView) {
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

    /// Page load / resume. If a resumable session exists — `running` or
    /// `paused`, the only states `mailbox-migration.md` § Resume protocol
    /// resumes — jumps straight to its Progress screen; the wizard chrome
    /// (Source screen) is otherwise the default. A `completed` session is
    /// deliberately not resumed: it would hold every later visit on Done, with no
    /// Source step to start the next import from. (The export twin does resume
    /// `completed` — a finished export still has a download to collect; see
    /// `mail-export.md`'s `list_export_sessions` row.)
    async fn refresh(&self) -> Result<(), DispatchError> {
        self.set_status(ImportStatus::Loading);
        let sessions = self.nest.list_sessions().await?;
        let active = sessions.into_iter().find(|s| {
            matches!(
                s.state,
                ImportSessionState::Running | ImportSessionState::Paused
            )
        });
        self.set_status(ImportStatus::Idle);
        if let Some(active) = active {
            self.store_session(active);
        }
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
    async fn connect(&self) -> Result<(), DispatchError> {
        self.set_status(ImportStatus::Working);
        let params = {
            let snap = self.inner.lock().expect("snapshot mutex");
            SourceConnectParams {
                kind: snap.source_kind,
                host: snap.host.clone(),
                port: snap.port,
                tls_mode: snap.tls_mode,
                username: snap.username.clone(),
                password: snap.password.clone(),
            }
        };
        if let Err(e) = self.source.connect(params).await {
            if !self.source.can_ever_connect() {
                self.inner.lock().expect("snapshot mutex").password = SecretString::default();
            }
            return Err(e.into());
        }
        let mailboxes = self.source.list_source_mailboxes().await?;
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.mailboxes = mailboxes
            .into_iter()
            .filter(|m| m.selectable)
            .map(|m| SourceMailboxOption {
                selected: default_selected(&m.name),
                name: m.name,
                message_count: m.message_count,
            })
            .collect();
        snap.password = SecretString::default();
        snap.step = ImportStep::Scope;
        snap.status = ImportStatus::Idle;
        Ok(())
    }

    async fn start(&self) -> Result<(), DispatchError> {
        self.set_status(ImportStatus::Working);
        let (descriptor, total_count, scope, date_from) = {
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
            .start_session(descriptor, total_count, scope, date_from)
            .await?;
        self.store_session(session);
        // A fresh run: no mailbox has been touched yet under this session.
        *self.cursors.lock().expect("cursors mutex") = HashMap::new();
        *self.error_budget.lock().expect("error budget mutex") = ErrorBudget::default();
        *self.stop.lock().expect("stop mutex") = StopRequest::None;
        self.set_status(ImportStatus::Idle);
        Ok(())
    }

    /// § UX shape step 5: `run_import`'s loop polls [`StopRequest`] between
    /// fetch windows and drains the in-flight one before actually stopping —
    /// this method itself only records the request and transitions the
    /// nest-side session; it does not block on the loop noticing.
    async fn pause(&self) -> Result<(), DispatchError> {
        self.set_status(ImportStatus::Working);
        let id = self.current_session_id()?;
        let session = self.nest.pause_session(id).await?;
        self.store_session(session);
        *self.stop.lock().expect("stop mutex") = StopRequest::Pause;
        self.set_status(ImportStatus::Idle);
        Ok(())
    }

    async fn resume(&self) -> Result<(), DispatchError> {
        self.set_status(ImportStatus::Working);
        let id = self.current_session_id()?;
        let session = self.nest.resume_session(id).await?;
        self.store_session(session);
        *self.stop.lock().expect("stop mutex") = StopRequest::None;
        self.set_status(ImportStatus::Idle);
        Ok(())
    }

    /// § UX shape step 5: cancel keeps already-imported messages, so — unlike
    /// export's `Cancel` — the wizard stays on the Progress screen (via
    /// `apply_session`) rather than resetting to step 1. `run_import`'s loop
    /// aborts the in-flight fetch window rather than draining it (see
    /// [`StopRequest::Cancel`]).
    async fn cancel(&self) -> Result<(), DispatchError> {
        self.set_status(ImportStatus::Working);
        let id = self.current_session_id()?;
        let session = self.nest.cancel_session(id).await?;
        self.store_session(session);
        *self.stop.lock().expect("stop mutex") = StopRequest::Cancel;
        self.set_status(ImportStatus::Idle);
        Ok(())
    }

    fn push_error_log(&self, line: String) {
        self.inner
            .lock()
            .expect("snapshot mutex")
            .error_log
            .push(line);
    }

    /// § Failure handling: "TCP/TLS error mid-session: up to 3 retries with
    /// exponential backoff (5 s, 30 s, 2 min) before moving the session to
    /// errored." Only [`NestError::Transient`] is retried — a
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

    /// Report a session-fatal condition to nest, surface it on the snapshot's
    /// `error-message`, and return it as the `run_import` result.
    async fn fail_and_report(&self, session_id: String, reason: String) -> DispatchError {
        if let Ok(session) = self.nest.fail_session(session_id, reason.clone()).await {
            self.store_session(session);
        }
        let _ = self.source.logout().await;
        let mut snap = self.inner.lock().expect("snapshot mutex");
        crate::state::set_snapshot_error(&mut snap.error, reason.clone());
        snap.status = ImportStatus::Idle;
        DispatchError::InvalidState(reason)
    }

    /// Send one packed unit and fold its outcomes into the rendered counters
    /// and the § Per-message error budget. Every lock here is taken and
    /// dropped synchronously, around the single `.await` — never held
    /// across it, or `run_import`'s future (spawned by
    /// `uniffi::export(async_runtime = "tokio")`) would stop being `Send`.
    /// Returns `true` when this call's outcomes breached the budget.
    async fn send_and_record(
        &self,
        session_id: &str,
        unit: ImportUnit,
        revised_total_count: Option<u64>,
    ) -> Result<bool, NestError> {
        let reply = self
            .nest
            .send_unit(session_id.to_string(), unit, false, revised_total_count)
            .await?;
        let mut breached = false;
        let mut snap = self.inner.lock().expect("snapshot mutex");
        let mut budget = self.error_budget.lock().expect("error budget mutex");
        for outcome in reply.outcomes {
            match outcome {
                SendOutcome::Imported => {
                    snap.imported_count += 1;
                    breached |= budget.record(false);
                }
                SendOutcome::Skipped { .. } => {
                    snap.skipped_count += 1;
                    breached |= budget.record(false);
                }
                SendOutcome::Errored { reason } => {
                    snap.errored_count += 1;
                    snap.error_log.push(reason);
                    breached |= budget.record(true);
                }
            }
        }
        Ok(breached)
    }

    /// One message's source-fetch failure (never reaches nest — no
    /// `import_message` call was ever made for it): logs it and records it
    /// against the error budget. Returns `true` on breach.
    fn record_source_fetch_failure(&self, mailbox: &str, uid: u32, reason: String) -> bool {
        self.push_error_log(format!("{mailbox} uid {uid}: {reason}"));
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
        let (mailboxes, date_from): (Vec<String>, String) = {
            let snap = self.inner.lock().expect("snapshot mutex");
            (
                snap.mailboxes
                    .iter()
                    .filter(|m| m.selected)
                    .map(|m| m.name.clone())
                    .collect(),
                snap.date_from.clone(),
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
        // The tightened estimate waiting for a call to carry it.
        let mut pending_revision: Option<u64> = None;

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

                let status = match self
                    .retrying(|| self.source.examine(&mailbox, cursor))
                    .await
                {
                    Ok(s) => s,
                    Err(NestError::Rejected(reason)) => {
                        self.push_error_log(format!("{mailbox}: {reason}"));
                        continue 'mailboxes;
                    }
                    Err(NestError::Transient(reason)) => {
                        return Err(self.fail_and_report(session_id, reason).await);
                    }
                };

                let outcomes = match self
                    .retrying(|| {
                        self.source
                            .fetch_window(&mailbox, cursor, FETCH_WINDOW_SIZE)
                    })
                    .await
                {
                    Ok(o) => o,
                    Err(NestError::Rejected(reason)) => {
                        self.push_error_log(format!("{mailbox}: {reason}"));
                        continue 'mailboxes;
                    }
                    Err(NestError::Transient(reason)) => {
                        return Err(self.fail_and_report(session_id, reason).await);
                    }
                };

                if outcomes.is_empty() {
                    // Mailbox exhausted.
                    continue 'mailboxes;
                }

                // StopRequest::Cancel aborts without draining this window —
                // the fetched-but-unsent messages are simply re-fetched (the
                // cursor below has not advanced) if the session is ever
                // resumed. Pause drains it, so the sent-so-far cursor is
                // always durable before the loop actually stops.
                let stop_now = *self.stop.lock().expect("stop mutex");
                if stop_now == StopRequest::Cancel {
                    break 'mailboxes;
                }

                // § Progress lives nest-side's `total_count`: the estimate
                // came from the source's `EXISTS`, which counts whole
                // mailboxes, and the range takes only part of them. Every
                // message the floor turns away is one the estimate should
                // never have held — so the revision is the session's CURRENT
                // total minus what this window excluded.
                //
                // RELATIVE, not absolute, and that is the whole point: a walk
                // resumed from a cursor cannot count what an earlier run
                // already saw (the cursor is a source UID, and UIDs are
                // sparse — they are not a count of anything), but it can
                // still say how many more the range has just ruled out. An
                // absolute figure here would drop the total below the
                // messages already imported under the same session.
                //
                // A source-side FETCH failure is NOT excluded mail: that
                // message was wanted, and it is counted in `errored_count`,
                // so taking it off the total would leave the total beneath
                // the outcomes already recorded against it.
                //
                // Folded over the WHOLE window before any of it is sent, so
                // the revision rides this window's own call — one computed
                // afterwards would ride the NEXT window's, and a mailbox that
                // ends in this one has no next window.
                if floor.is_bounded() {
                    let excluded = outcomes
                        .iter()
                        .filter(|o| match o {
                            FetchOutcome::Fetched(m) => !floor.admits(m.internal_date_epoch),
                            FetchOutcome::Failed { .. } => false,
                        })
                        .count() as u64;
                    if excluded > 0 {
                        let mut snap = self.inner.lock().expect("snapshot mutex");
                        // Applied locally as well as sent, so consecutive
                        // windows chain off the tightened figure rather than
                        // each subtracting from the original estimate.
                        snap.total_count = snap.total_count.saturating_sub(excluded);
                        pending_revision = Some(snap.total_count);
                    }
                }

                let mut packer = BatchPacker::new();
                let mut highest = cursor;
                let mut breached = false;
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
                                breached |= self
                                    .send_and_record(&session_id, unit, pending_revision.take())
                                    .await?;
                                if breached {
                                    break;
                                }
                            }
                        }
                        FetchOutcome::Failed { uid, reason } => {
                            highest = Some(MailboxCursor {
                                last_processed_source_uid: uid,
                                source_uid_validity: status.uid_validity,
                            });
                            breached |= self.record_source_fetch_failure(&mailbox, uid, reason);
                        }
                    }
                    if breached {
                        break;
                    }
                }
                if !breached && let Some(unit) = packer.flush() {
                    breached |= self
                        .send_and_record(&session_id, unit, pending_revision.take())
                        .await?;
                }

                if let Some(c) = highest {
                    self.cursors
                        .lock()
                        .expect("cursors mutex")
                        .insert(mailbox.clone(), c);
                }

                if breached {
                    return Err(self
                        .fail_and_report(session_id, "per-message error budget exceeded".into())
                        .await);
                }
                if stop_now == StopRequest::Pause {
                    break 'mailboxes;
                }
            }
        }

        if *self.stop.lock().expect("stop mutex") == StopRequest::None {
            let session = self.nest.finalize_session(session_id).await?;
            self.store_session(session);
            let _ = self.source.logout().await;
        }
        Ok(())
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
            MailImportAction::Next => snap.step = next_step(snap.step),
            MailImportAction::Back => snap.step = prev_step(snap.step),
            _ => return false,
        }
        true
    }
}

/// Scope → Confirm (Confirm is the last client-side step; `Start` drives
/// past it). Source → Scope only happens via a successful `Connect`.
fn next_step(step: ImportStep) -> ImportStep {
    match step {
        ImportStep::Scope => ImportStep::Confirm,
        other => other,
    }
}

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
        let result = self.run_import_inner().await;
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
                MailImportAction::Start => self.start().await,
                MailImportAction::Pause => self.pause().await,
                MailImportAction::Resume => self.resume().await,
                MailImportAction::Cancel => self.cancel().await,
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
    use fauna_protocol::bridge_routing::ImportMessageItem;
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
    }

    impl FakeNest {
        fn new() -> Self {
            Self {
                session: StdMutex::new(None),
                sent_uids: StdMutex::new(Vec::new()),
                error_uids: StdMutex::new(HashSet::new()),
            }
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
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl MailImportNest for FakeNest {
        async fn list_sessions(&self) -> Result<Vec<ImportSessionView>, NestError> {
            Ok(self.session.lock().unwrap().clone().into_iter().collect())
        }
        async fn start_session(
            &self,
            _source_descriptor: String,
            total_count: u64,
            scope: Vec<String>,
            date_from: String,
        ) -> Result<ImportSessionView, NestError> {
            let mut s = session(ImportSessionState::Running, total_count);
            s.scope = scope;
            s.date_from = date_from;
            *self.session.lock().unwrap() = Some(s.clone());
            Ok(s)
        }
        async fn pause_session(&self, _session_id: String) -> Result<ImportSessionView, NestError> {
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
            let mut guard = self.session.lock().unwrap();
            let mut s = guard
                .clone()
                .ok_or(NestError::Rejected("no session".into()))?;
            s.state = ImportSessionState::Cancelled;
            *guard = Some(s.clone());
            Ok(s)
        }
        async fn finalize_session(
            &self,
            _session_id: String,
        ) -> Result<ImportSessionView, NestError> {
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
            _skip_dedup: bool,
            revised_total_count: Option<u64>,
        ) -> Result<SendUnitReply, NestError> {
            let items: Vec<ImportMessageItem> = match unit {
                ImportUnit::Batch(items) => items,
                ImportUnit::Single(item) => vec![*item],
            };
            let error_uids = self.error_uids.lock().unwrap();
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
                    if error_uids.contains(&item.source_uid) {
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
    }

    /// In-memory foreign IMAP source.
    struct FakeSource {
        mailboxes: Vec<SourceMailboxView>,
        /// Set to force `connect` to fail (bad credentials).
        reject: bool,
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
        logged_out: StdMutex<bool>,
        /// `can_ever_connect`'s answer — models the web stub.
        can_never_connect: bool,
    }

    impl FakeSource {
        fn new(mailboxes: &[(&str, bool, u32)]) -> Self {
            Self {
                mailboxes: mailboxes
                    .iter()
                    .map(|(name, selectable, count)| SourceMailboxView {
                        name: name.to_string(),
                        selectable: *selectable,
                        message_count: *count,
                    })
                    .collect(),
                reject: false,
                uid_validity: 7,
                messages: HashMap::new(),
                fail_uids: HashSet::new(),
                examine_errors: StdMutex::new(HashMap::new()),
                fetch_errors: StdMutex::new(HashMap::new()),
                logged_out: StdMutex::new(false),
                can_never_connect: false,
            }
        }

        fn rejecting() -> Self {
            Self {
                mailboxes: Vec::new(),
                reject: true,
                ..Self::new(&[])
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

        fn failing_uid(mut self, uid: u32) -> Self {
            self.fail_uids.insert(uid);
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
        async fn connect(&self, params: SourceConnectParams) -> Result<(), NestError> {
            if self.reject {
                return Err(NestError::Rejected("bad credentials".into()));
            }
            assert!(!params.username.is_empty(), "connect needs a username");
            Ok(())
        }
        async fn list_source_mailboxes(&self) -> Result<Vec<SourceMailboxView>, NestError> {
            Ok(self.mailboxes.clone())
        }
        async fn examine(
            &self,
            mailbox: &str,
            cursor: Option<MailboxCursor>,
        ) -> Result<MailboxStatus, NestError> {
            if let Some(e) = self
                .examine_errors
                .lock()
                .unwrap()
                .get_mut(mailbox)
                .and_then(VecDeque::pop_front)
            {
                return Err(e);
            }
            if let Some(c) = cursor
                && c.source_uid_validity != self.uid_validity
            {
                return Err(NestError::Rejected(format!(
                    "{mailbox}: source UIDVALIDITY changed"
                )));
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
        ) -> Result<Vec<FetchOutcome>, NestError> {
            if let Some(e) = self
                .fetch_errors
                .lock()
                .unwrap()
                .get_mut(mailbox)
                .and_then(VecDeque::pop_front)
            {
                return Err(e);
            }
            let from = cursor.map_or(1, |c| c.last_processed_source_uid + 1);
            let outcomes = self
                .messages
                .get(mailbox)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|m| m.uid >= from)
                .take(max)
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
            Ok(outcomes)
        }
        async fn logout(&self) -> Result<(), NestError> {
            *self.logged_out.lock().unwrap() = true;
            Ok(())
        }

        fn can_ever_connect(&self) -> bool {
            !self.can_never_connect
        }
    }

    /// An injectable [`RetryClock`] that records requested delays instead of
    /// waiting (`testing.md` convention 14).
    struct FakeClock {
        slept: StdMutex<Vec<u64>>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                slept: StdMutex::new(Vec::new()),
            }
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl RetryClock for FakeClock {
        async fn sleep_ms(&self, ms: u64) {
            self.slept.lock().unwrap().push(ms);
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
    /// `Gmail`/`ICloud` and relies entirely on this arm to have run. `Outlook`
    /// is not in that set — it always sends whatever host the caller passes
    /// (`connect_actions`'s own doc comment). A user
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

    #[test]
    fn start_args_never_includes_the_password() {
        let mut snap = MailImportSnapshot::empty();
        snap.username = "user@example.com".into();
        snap.password = "super-secret".into();
        snap.mailboxes = vec![SourceMailboxOption {
            name: "INBOX".into(),
            selected: true,
            message_count: 7,
        }];
        snap.date_from = "2023-11-14".into();
        let (descriptor, total, scope, date_from) = snap.start_args();
        assert_eq!(descriptor, "gmail:user@example.com@imap.gmail.com");
        assert!(!descriptor.contains("super-secret"));
        assert_eq!(total, 7);
        assert_eq!(scope, vec!["INBOX".to_string()]);
        assert_eq!(date_from, "2023-11-14");
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

    /// Pin for `connect_actions`'s own doc comment: `Outlook` is an IMAP fallback beside its disabled OAuth
    /// button, not a preset kind — it must send whatever host/port the
    /// caller passes, never the snapshot's `outlook.office365.com`. The
    /// commit that first documented this precondition named
    /// `Outlook` alongside `Gmail`/`ICloud` in both the doc comment and
    /// `mailbox-migration.md`, backwards; `connect_actions_drops_an_unparseable_port`
    /// below already exercises this arm incidentally (its own purpose is the
    /// unparseable-port fallback), so this test is the intent-named
    /// complement to `connect_actions_for_gmail_omits_host_and_port`, not
    /// the walk's first coverage.
    #[test]
    fn connect_actions_for_outlook_sends_host_and_port() {
        let actions = connect_actions(
            ImportSourceKind::Outlook,
            "  imap.outlook-fallback.example  ",
            "993",
            "  user@outlook.example  ",
            SecretString::from("pw".to_string()),
        );
        assert_eq!(
            actions,
            vec![
                MailImportAction::SetHost {
                    value: "imap.outlook-fallback.example".into()
                },
                MailImportAction::SetPort { value: 993 },
                MailImportAction::SetUsername {
                    value: "user@outlook.example".into()
                },
                MailImportAction::SetPassword {
                    value: SecretString::from("pw".to_string())
                },
                MailImportAction::Connect,
            ]
        );
    }

    #[test]
    fn connect_actions_drops_an_unparseable_port() {
        let actions = connect_actions(
            ImportSourceKind::Outlook,
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
        let m = MailImportMachine::new(nest, Arc::new(source), clock);
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
        // first call; Work exhausts its retries and fails the session. A
        // second `run_import` call (no fresh `Start`) must not re-send
        // INBOX's already-sent messages — only its own cursor, tracked on
        // the machine, prevents that.
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

        let first = m.run_import().await;
        assert!(
            first.is_err(),
            "Work's retries must exhaust and fail the run"
        );
        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 2, 3]);
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Errored
        );
        // 3 retries (the 4th queued failure is never reached — the 3rd
        // exhausts RETRY_DELAYS_MS).
        assert_eq!(clock.slept.lock().unwrap().as_slice(), &RETRY_DELAYS_MS);

        // The app calls run_import again; Work's error queue is now drained,
        // so this call succeeds — and must not re-send INBOX's 3 messages.
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

    #[tokio::test]
    async fn a_uidvalidity_mismatch_skips_the_mailbox_without_failing_the_session() {
        // § Resume protocol step 4 / § Failure handling: per-mailbox, not
        // session-fatal. Modeled here as the very first `examine` call
        // (cursor `None`) still hitting the mismatch — the fake's mismatch
        // check only fires with a cursor, so inject it as a `Rejected`
        // directly via the examine-error queue instead.
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3), ("Work", true, 2)])
            .with_messages("INBOX", 3)
            .with_messages("Work", 2)
            .queue_examine_error(
                "INBOX",
                NestError::Rejected("INBOX: source UIDVALIDITY changed".into()),
            );
        let m = started(nest.clone(), source, Arc::new(FakeClock::new())).await;

        m.run_import().await.unwrap();

        // INBOX skipped entirely; Work still fully imports.
        assert_eq!(nest.sent_uids.lock().unwrap().as_slice(), &[1, 2]);
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().state,
            ImportSessionState::Completed
        );
        assert!(m.snapshot().error_log.iter().any(|l| l.contains("INBOX")));
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

    #[tokio::test]
    async fn exhausting_retries_fails_the_session_with_the_transient_reason() {
        let nest = Arc::new(FakeNest::new());
        let source = FakeSource::new(&[("INBOX", true, 3)])
            .with_messages("INBOX", 3)
            .queue_examine_error("INBOX", NestError::Transient("t1".into()))
            .queue_examine_error("INBOX", NestError::Transient("t2".into()))
            .queue_examine_error("INBOX", NestError::Transient("t3".into()))
            .queue_examine_error("INBOX", NestError::Transient("t4".into()));
        let clock = Arc::new(FakeClock::new());
        let m = started(nest.clone(), source, clock.clone()).await;

        let result = m.run_import().await;

        assert!(result.is_err());
        assert!(nest.sent_uids.lock().unwrap().is_empty());
        assert_eq!(clock.slept.lock().unwrap().as_slice(), &RETRY_DELAYS_MS);
        let session = nest.session.lock().unwrap().clone().unwrap();
        assert_eq!(session.state, ImportSessionState::Errored);
        assert!(
            session.error_reason.contains("t4"),
            "{}",
            session.error_reason
        );
        assert!(m.snapshot().error.is_some());
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

    /// Drive a machine to Start with a since date already typed. Mirrors
    /// `started`, which cannot take one: the field belongs to step 3, which
    /// `started`'s second `Next` has already left.
    async fn started_since(
        nest: Arc<FakeNest>,
        source: FakeSource,
        since: &str,
    ) -> Result<MailImportMachine, DispatchError> {
        let m = MailImportMachine::new(nest, Arc::new(source), Arc::new(FakeClock::new()));
        connected(&m).await;
        m.dispatch(MailImportAction::Next).await.unwrap();
        set_since(&m, since).await;
        m.dispatch(MailImportAction::Next).await.unwrap();
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

    /// A malformed date refuses `Start` before any session exists — never
    /// "no range", which would silently import the whole mailbox.
    #[tokio::test]
    async fn a_malformed_since_date_refuses_start_before_opening_a_session() {
        for bad in ["2023-13-01", "2023-11-31", "14/11/2023", "2023-11", "nope"] {
            let nest = Arc::new(FakeNest::new());
            let source = FakeSource::new(&[("INBOX", true, 1)]).with_messages("INBOX", 1);
            let err = started_since(nest.clone(), source, bad)
                .await
                .err()
                .unwrap_or_else(|| panic!("{bad} must refuse Start"));
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

    /// The estimate must survive a Pause. A revision computed as an ABSOLUTE
    /// figure cannot: a resumed walk restarts its own counters while the
    /// cursor means it re-fetches only the tail, so it would publish a total
    /// beneath the messages the session had already imported — a progress bar
    /// reading past 100% and a row that lies to the user's other devices.
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
            64,
            "starts at the source's EXISTS"
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
            "and the estimate lands on exactly them, across the resume"
        );
        assert_eq!(
            nest.session.lock().unwrap().as_ref().unwrap().total_count,
            32,
            "the ROW carries it too — a reload and the user's other devices              read the row, not this machine's snapshot"
        );
    }

    /// § Progress lives nest-side's `total_count`: the source's `EXISTS`
    /// counts a whole mailbox, so under a since date the estimate starts
    /// overstated and must come down to the truth.
    #[tokio::test]
    async fn a_since_date_stops_total_count_overstating_once_the_walk_finishes() {
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
            4,
            "the estimate starts at the source's whole-mailbox EXISTS"
        );

        m.run_import().await.unwrap();

        assert_eq!(
            m.snapshot().total_count,
            2,
            "and lands on what the range actually admitted"
        );
    }
}
