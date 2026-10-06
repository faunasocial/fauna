//! Shared state machine for the user-facing `mail-aliases` page — a person
//! managing **their own** per-account mail addresses (`mail-aliases.md`
//! § Aliases UX), a sub-page of the `mail-settings` family.
//!
//! Authority for behavior + the alias-kind taxonomy + the per-alias controls:
//! `docs/goal/behavior/mail-aliases.md`. Authority for UX/IDs:
//! `tests/e2e-unified/ui.yaml` (`mail-aliases` page + `mail-aliases-list`
//! component).
//!
//! Per priority #2/#4 the snapshot projection (`AliasRow` → [`AliasView`] with
//! the kind-aware address render + the `default_domain` derivation), the
//! add-sheet **pre-validation** (reusing the pure `fauna_mail::aliases`
//! validators the nest itself uses — `mail-aliases.md` § aliases feature), and
//! the action sequencing (re-read after every mutation, hex-decode the alias id
//! for the wire) live here, not in any per-app shell. The UI renders
//! [`MailAliasesSnapshot`] and dispatches [`MailAliasesAction`]; the per-app
//! glue implements one WS-RPC seam ([`MailAliasesNest`]) over the **User-class**
//! `MailAccountClient` (`libs/fauna-client-bridges`). Mirrors `forwarders.rs`
//! (the admin sibling) precisely.
//!
//! **Distinct from `forwarders.rs`:** that machine drives the Admin-class
//! `MailAdminClient` (external forwarders are deployment config); this drives the
//! User-class `MailAccountClient` (a user touches only their own aliases — the
//! nest derives the owning actor from the authenticated caller).
//!
//! **Re-enable** is wired: `revoke` sets `disabled=true` and `enable`
//! (`fauna.bridges.enable_account_alias`) sets it back to `false`, so the
//! disabled-toggle is two-way — disable is no longer a one-way trap
//! (`mail-aliases.md:156`). One slice lands later (UI is forward-compatible):
//! the per-alias **audit** disclosure (`mail-aliases-list-item-show-audit` →
//! `list_account_alias_hits`).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_core::localized::LocalizedText;
use fauna_mail::aliases::{
    ALIAS_KIND_DISPOSABLE, ALIAS_KIND_EXACT, ALIAS_KIND_FORWARDER, ALIAS_KIND_WILDCARD_PREFIX,
    DEFAULT_RESERVED_LOCAL_PARTS, validate_exact_local_part, validate_wildcard_prefix,
};
use fauna_protocol::MaybeSendSync;
use fauna_protocol::bridge_routing::{
    AliasControls, AliasRow, ImportAliasOutcome, ImportAliasStatus,
};
use serde::{Deserialize, Serialize};

use crate::error::{DispatchError, NestError};

/// `+suffix` (RFC 5233) is resolver-only — never a stored row (`mail-aliases.md`
/// § Kind 2). Subaddressing therefore never appears in `list_account_aliases`;
/// the variant exists only so a future wire kind can't silently fall through.
const ALIAS_KIND_SUBADDRESS: &str = "subaddress";

/// The kind badge the `mail-aliases-list-item-kind` element renders, projected
/// from the wire `kind` string. The canonical variant→badge map is
/// [`alias_kind_badge`], returning [`LocalizedText`] so each app resolves it
/// through its own i18n runtime (the shared machine stays locale-free, mirroring
/// `state::CredentialKind` / [`member_status_label`](crate::member_status_label)).
/// `list_account_aliases` is owner-scoped and excludes forwarders, so in practice
/// only `Exact` / `Wildcard` / `Disposable` reach the list — the others round out
/// the taxonomy for forward-compatibility.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum AliasKind {
    Exact,
    Subaddress,
    Wildcard,
    Disposable,
    Catchall,
    Forwarder,
    /// An unrecognized wire kind — rendered as a neutral badge rather than
    /// dropped, so a nest that grows a kind doesn't make rows vanish.
    Other,
}

impl AliasKind {
    /// Project the wire `kind` string to the badge variant.
    pub fn from_wire(kind: &str) -> Self {
        match kind {
            k if k == ALIAS_KIND_EXACT => Self::Exact,
            k if k == ALIAS_KIND_SUBADDRESS => Self::Subaddress,
            k if k == ALIAS_KIND_WILDCARD_PREFIX => Self::Wildcard,
            k if k == ALIAS_KIND_DISPOSABLE => Self::Disposable,
            "catchall" => Self::Catchall,
            k if k == ALIAS_KIND_FORWARDER => Self::Forwarder,
            _ => Self::Other,
        }
    }

    /// The wire `kind` string for a create action. `None` for the kinds a user
    /// cannot create via `create_account_alias` (disposable mints via its own
    /// RPC; subaddress is resolver-only; catch-all/forwarder are admin-tier).
    fn create_wire(self) -> Option<&'static str> {
        match self {
            Self::Exact => Some(ALIAS_KIND_EXACT),
            Self::Wildcard => Some(ALIAS_KIND_WILDCARD_PREFIX),
            _ => None,
        }
    }
}

/// Canonical label for an [`AliasKind`], returned as [`LocalizedText`] so each
/// app resolves it through its own i18n runtime (mirrors
/// [`member_status_label`](crate::member_status_label) /
/// [`bridge_display_name`](crate::bridge_display_name)). Lifts the identical
/// seven-arm map that linux/windows/apple/android each hard-coded for the
/// `mail-aliases-list-item-kind` badge — one source of truth (priority #1/#2).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn alias_kind_badge(kind: AliasKind) -> LocalizedText {
    match kind {
        AliasKind::Exact => LocalizedText::key("mail_aliases.kind_exact"),
        AliasKind::Subaddress => LocalizedText::key("mail_aliases.kind_subaddress"),
        AliasKind::Wildcard => LocalizedText::key("mail_aliases.kind_wildcard"),
        AliasKind::Disposable => LocalizedText::key("mail_aliases.kind_disposable"),
        AliasKind::Catchall => LocalizedText::key("mail_aliases.kind_catchall"),
        AliasKind::Forwarder => LocalizedText::key("mail_aliases.kind_forwarder"),
        AliasKind::Other => LocalizedText::key("mail_aliases.kind_other"),
    }
}

/// Canonical text for the `mail-aliases-list-item-hits` row — the hit count,
/// plus (when present) a last-hit date. Returned as [`LocalizedText`] so each
/// app resolves the i18n template (`{count}` / `{date}` placeholders),
/// lifting the identical English-literal `"{n} hits"` / `"{n} hits · last
/// {date}"` text that linux/windows/apple/web each hard-coded (priority #1
/// untranslated-English + #2 single-source; mirrors [`alias_kind_badge`]).
///
/// The `last_hit_date` string is passed **in** pre-formatted: the calendar date
/// depends on the viewer's *local* timezone, a native concern (each app
/// formats it with its own locale-/tz-aware date API), so only the surrounding
/// template is shared here.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn alias_hits_label(hit_count: u64, last_hit_date: Option<String>) -> LocalizedText {
    match last_hit_date {
        Some(date) => {
            let mut text = LocalizedText::key_arg(
                "mail_aliases.hits_with_last",
                "count",
                hit_count.to_string(),
            );
            text.args.insert("date".to_string(), date);
            text
        }
        None => LocalizedText::key_arg("mail_aliases.hits", "count", hit_count.to_string()),
    }
}

/// One alias row as `mail-aliases-list` renders it. Projected from the wire
/// [`AliasRow`]. Carries the editable controls so the Edit sheet pre-populates
/// (`update_account_alias` is full-overwrite — `mail-aliases.md:209` — so the UI
/// submits the complete control set from the row it edits, and the machine
/// preserves the controls the seed add-sheet doesn't expose).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AliasView {
    /// Lowercase hex of the 16-byte alias id; carried verbatim into the
    /// update/revoke/delete actions (the machine hex-decodes for the wire).
    pub alias_id_hex: String,
    pub local_domain: String,
    pub kind: AliasKind,
    /// The raw wire `pattern` (localpart for exact/wildcard; the base32 token
    /// for disposable — `mail-aliases.md` § Storage).
    pub pattern: String,
    /// Kind-aware display for `mail-aliases-list-item-pattern`: `bob@d` (exact),
    /// `bob-*@d` (wildcard prefix incl. its trailing `-`), `…-temp-<token>@d`
    /// (disposable — the recognizable `-temp-` tag; the full handle-bearing
    /// address was copied at mint time).
    pub address: String,
    /// `mail-aliases-list-item-label` (empty = no label).
    pub label: String,
    /// `mail-aliases-list-item-disabled-toggle`.
    pub disabled: bool,
    /// Whether this is the actor's canonical `<handle>@<domain>` exact alias —
    /// the primary mailbox + AUTH-login identity, computed nest-side from the
    /// runtime mail domain (wire `AliasRow.is_canonical`). When set, the UI
    /// renders the row **read-only** — no disable/delete/edit, marked as the
    /// primary address (`mail-aliases.md` § Aliases UX) — so the nest's
    /// canonical-protection guard is visible rather than only an error on
    /// attempt.
    pub is_canonical: bool,
    /// Part of `mail-aliases-list-item-hits`. Clamped from the wire `i64`.
    pub hit_count: u64,
    /// Part of `mail-aliases-list-item-hits` (epoch-millis; `None` = never hit).
    pub last_hit_at_ms: Option<i64>,
    /// Edit-sheet prepopulate (`mail-aliases-add-sheet-spam-threshold-input`).
    pub spam_threshold_override: Option<u32>,
    /// Edit-sheet prepopulate (`mail-aliases-add-sheet-rate-per-hour-input`).
    pub rate_limit_per_hour: Option<i64>,
    /// Not exposed in the seed add-sheet; **preserved across an update** so a
    /// full-overwrite doesn't silently drop it.
    pub rate_limit_per_day: Option<i64>,
    /// Disposable-only remaining uses (`None` = unlimited / non-disposable).
    pub uses_remaining: Option<i64>,
    /// Disposable-only epoch-millis expiry (`None` = no expiry).
    pub expires_at_ms: Option<i64>,
}

/// Render the kind-aware display address for `mail-aliases-list-item-pattern`.
fn render_address(kind: AliasKind, pattern: &str, local_domain: &str) -> String {
    match kind {
        // The stored prefix includes its trailing `-` (e.g. `bob-`); show the
        // glob the user reasons about.
        AliasKind::Wildcard => format!("{pattern}*@{local_domain}"),
        // Disposable `pattern` is the bare token; surface the `-temp-` tag so
        // the row is recognizable as a disposable at a glance.
        AliasKind::Disposable => {
            format!(
                "{}{pattern}@{local_domain}",
                fauna_mail::aliases::DISPOSABLE_INFIX
            )
        }
        _ => format!("{pattern}@{local_domain}"),
    }
}

impl From<AliasRow> for AliasView {
    fn from(row: AliasRow) -> Self {
        let kind = AliasKind::from_wire(&row.kind);
        let address = render_address(kind, &row.pattern, &row.local_domain);
        Self {
            alias_id_hex: hex::encode(&row.alias_id),
            local_domain: row.local_domain,
            kind,
            pattern: row.pattern,
            address,
            label: row.label,
            disabled: row.disabled,
            is_canonical: row.is_canonical,
            hit_count: row.hit_count.max(0) as u64,
            last_hit_at_ms: row.last_hit_at,
            spam_threshold_override: row.spam_threshold_override,
            rate_limit_per_hour: row.rate_limit_per_hour,
            rate_limit_per_day: row.rate_limit_per_day,
            uses_remaining: row.uses_remaining,
            expires_at_ms: row.expires_at,
        }
    }
}

/// One line's import outcome as the `mail-aliases-import-result` renders it.
/// Local uniffi/serde view mirroring the wire `ImportAliasOutcome` (the wire
/// enum is not uniffi-scaffolded), matching how `AliasView` re-derives from the
/// wire `AliasRow`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ImportAliasStatusView {
    Created,
    SkippedDuplicate,
    Invalid,
}

impl From<ImportAliasStatus> for ImportAliasStatusView {
    fn from(s: ImportAliasStatus) -> Self {
        match s {
            ImportAliasStatus::Created => Self::Created,
            ImportAliasStatus::SkippedDuplicate => Self::SkippedDuplicate,
            // A status a newer nest added: not created, so it reads as a line
            // the import rejected, and the client lists its reason.
            ImportAliasStatus::Invalid | ImportAliasStatus::Unknown => Self::Invalid,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ImportAliasOutcomeView {
    pub line_index: u32,
    pub address: String,
    pub status: ImportAliasStatusView,
    /// Short human reason on `Invalid`/`SkippedDuplicate`, `None` on `Created`.
    pub reason: Option<String>,
}

impl From<ImportAliasOutcome> for ImportAliasOutcomeView {
    fn from(o: ImportAliasOutcome) -> Self {
        Self {
            line_index: o.line_index,
            address: o.address,
            status: o.status.into(),
            reason: o.reason,
        }
    }
}

/// The whole import outcome the client renders: counts for the summary line
/// (`N created · M already existed · K invalid`) + the per-line outcomes (the
/// client lists the `Invalid` reasons).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ImportResultView {
    pub created: u32,
    pub skipped_duplicate: u32,
    pub invalid: u32,
    pub outcomes: Vec<ImportAliasOutcomeView>,
}

impl From<Vec<ImportAliasOutcome>> for ImportResultView {
    fn from(outcomes: Vec<ImportAliasOutcome>) -> Self {
        let mut created = 0;
        let mut skipped_duplicate = 0;
        let mut invalid = 0;
        for o in &outcomes {
            match o.status {
                ImportAliasStatus::Created => created += 1,
                ImportAliasStatus::SkippedDuplicate => skipped_duplicate += 1,
                ImportAliasStatus::Invalid | ImportAliasStatus::Unknown => invalid += 1,
            }
        }
        Self {
            created,
            skipped_duplicate,
            invalid,
            outcomes: outcomes
                .into_iter()
                .map(ImportAliasOutcomeView::from)
                .collect(),
        }
    }
}

/// Coarse machine status for spinner / disabled-control rendering. Mirrors
/// `ForwarderStatus`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum AliasesStatus {
    Idle,
    Loading,
    Working,
}

/// Read-only snapshot the per-app UI renders for `mail-aliases`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailAliasesSnapshot {
    /// The caller's own aliases (one `mail-aliases-list` row each), newest the
    /// wire order (the handler returns `(actor_id, kind)`-ordered rows).
    pub aliases: Vec<AliasView>,
    /// The `local_domain` a new exact/wildcard alias is created on. The
    /// `mail-aliases` add-sheet has **no** domain picker (`ui.yaml:853-866`), so
    /// the machine derives it from the user's canonical (oldest) exact alias —
    /// mirroring how the nest derives the domain for a disposable mint
    /// (`mail-aliases.md:471`). `None` ⇒ the user has no exact alias yet, so
    /// create/generate are unavailable (the nest would `no_canonical_address`);
    /// the UI disables the add controls.
    pub default_domain: Option<String>,
    /// Set after a successful `GenerateDisposable` to the full minted address —
    /// the UI copies it to the clipboard + toasts (`mail-aliases.md:263`).
    /// Cleared at the start of the next dispatch.
    pub last_minted_address: Option<String>,
    /// Set after a successful `Import` to the per-line outcome summary — the UI
    /// renders `mail-aliases-import-result`. Cleared at the start of the next
    /// dispatch (like `last_minted_address`).
    pub last_import_result: Option<ImportResultView>,
    pub status: AliasesStatus,
    /// Last action's error, surfaced via the `error-message` element
    /// (`conflicts_with_existing_alias` / `reserved_local_part` / validation
    /// failures, etc.). Cleared at the start of the next dispatch.
    pub error: Option<String>,
}

impl MailAliasesSnapshot {
    fn empty() -> Self {
        Self {
            aliases: Vec::new(),
            default_domain: None,
            last_minted_address: None,
            last_import_result: None,
            status: AliasesStatus::Idle,
            error: None,
        }
    }
}

/// Actions the per-app UI dispatches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum MailAliasesAction {
    /// Re-read the alias list (page load / after a mutation).
    Refresh,
    /// Create an `Exact` or `Wildcard` alias on the snapshot's `default_domain`.
    /// The machine pre-validates `pattern` with the shared
    /// `fauna_mail::aliases` validators; the nest re-validates + enforces
    /// cross-user uniqueness / caps. For `Wildcard`, a trailing `*` the user
    /// typed is stripped before validating/sending (the wire `pattern` is the
    /// literal prefix incl. its trailing `-`).
    Create {
        kind: AliasKind,
        pattern: String,
        label: String,
        spam_threshold_override: Option<u32>,
        rate_limit_per_hour: Option<i64>,
    },
    /// Mint a disposable alias (`mail-aliases.md` § Kind 5). `ttl_days` / `uses`
    /// `None` = the per-user default (30 days / 1 use); `uses = Some(0)` =
    /// unlimited. On success the snapshot's `last_minted_address` is set.
    GenerateDisposable {
        ttl_days: Option<u32>,
        uses: Option<u32>,
        label: String,
    },
    /// Full-overwrite the editable fields of an owned alias (`kind` is
    /// immutable). The seed UI edits `pattern` + `label` + spam-threshold +
    /// per-hour rate; the machine preserves the row's `rate_limit_per_day`
    /// (not an add-sheet field) so the overwrite doesn't drop it.
    Update {
        alias_id_hex: String,
        pattern: String,
        label: String,
        spam_threshold_override: Option<u32>,
        rate_limit_per_hour: Option<i64>,
    },
    /// Soft-off: flip `disabled = true` (preserves the row + audit). Backs both
    /// the revoke button and the disabled-toggle's disable direction. Reversible
    /// via [`MailAliasesAction::Enable`].
    Revoke { alias_id_hex: String },
    /// Re-enable: flip `disabled = false` — the reverse of `Revoke`. Backs the
    /// disabled-toggle's enable direction so disable is no longer a one-way trap
    /// (`mail-aliases.md:156`).
    Enable { alias_id_hex: String },
    /// Destructive irreversible remove (overflow-menu Delete).
    Delete { alias_id_hex: String },
    /// Bulk-import exact aliases from pasted lines (`mail-aliases.md` § Bulk
    /// import). Best-effort per line; the per-line outcomes land in the
    /// snapshot's `last_import_result` and the list refreshes.
    Import { lines: Vec<String> },
}

/// WS-RPC seam to nest. Per-app glue implements this over the User-class
/// `MailAccountClient` (`libs/fauna-client-bridges`) — each method a 1:1
/// forward. Dual `async_trait` arm + `MaybeSendSync` so the one seam serves
/// native + wasm (mirrors `ForwarderNest`).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait MailAliasesNest: MaybeSendSync {
    /// `fauna.bridges.list_account_aliases` — the caller's own rows.
    async fn list_account_aliases(&self) -> Result<Vec<AliasRow>, NestError>;
    /// `fauna.bridges.create_account_alias` (`kind` is `"exact"`/`"wildcard_prefix"`).
    async fn create_account_alias(
        &self,
        kind: String,
        local_domain: String,
        pattern: String,
        controls: AliasControls,
    ) -> Result<(), NestError>;
    /// `fauna.bridges.update_account_alias` (full-overwrite; `kind` immutable).
    async fn update_account_alias(
        &self,
        alias_id: Vec<u8>,
        pattern: String,
        controls: AliasControls,
    ) -> Result<(), NestError>;
    /// `fauna.bridges.revoke_account_alias` — flip `disabled = true`.
    async fn revoke_account_alias(&self, alias_id: Vec<u8>) -> Result<(), NestError>;
    /// `fauna.bridges.enable_account_alias` — flip `disabled = false` (reverse of revoke).
    async fn enable_account_alias(&self, alias_id: Vec<u8>) -> Result<(), NestError>;
    /// `fauna.bridges.delete_account_alias` — destructive remove.
    async fn delete_account_alias(&self, alias_id: Vec<u8>) -> Result<(), NestError>;
    /// `fauna.bridges.generate_disposable_alias` — returns the full minted
    /// address (`<handle>-temp-<token>@<domain>`) for the clipboard.
    async fn generate_disposable_alias(
        &self,
        ttl_days: Option<u32>,
        uses: Option<u32>,
        label: String,
    ) -> Result<String, NestError>;
    /// `fauna.bridges.import_account_aliases` — bulk-create exact aliases;
    /// returns the per-line outcomes.
    async fn import_account_aliases(
        &self,
        lines: Vec<String>,
    ) -> Result<Vec<ImportAliasOutcome>, NestError>;
}

/// Decode a row's hex alias id to the 16-byte wire form. A corrupted snapshot
/// surfaces as a user-visible error rather than panicking / sending garbage.
fn decode_alias_id(alias_id_hex: &str) -> Result<Vec<u8>, DispatchError> {
    crate::error::decode_hex_id16("alias id", alias_id_hex)
}

/// The canonical local-domain a new alias is created on: the `local_domain` of
/// the user's **canonical** exact alias — `<handle>@<domain>`, the row the
/// nest itself flags `is_canonical` (`mail-aliases.md` § Kind 1, § Aliases
/// UX). Falls back to any exact alias, then to any alias at all, if no row is
/// flagged canonical (a handle-less actor with locally-registered exacts, or
/// a fixture that never set the flag) — purely defensive; in practice the
/// canonical exact alias is created at mail-enable and always present.
///
/// MUST NOT pick by `alias_id_hex` — it is a random UUIDv4
/// (`bins/fauna-nest/src/db/mail_aliases.rs`), not time-sortable, so a
/// `min_by_key` over it selects an effectively random domain once a user has
/// exact aliases on more than one domain (this shipped as
/// "oldest exact alias" but never actually found the oldest one).
fn derive_default_domain(aliases: &[AliasView]) -> Option<String> {
    aliases
        .iter()
        .find(|a| a.is_canonical)
        .or_else(|| aliases.iter().find(|a| a.kind == AliasKind::Exact))
        .or_else(|| aliases.first())
        .map(|a| a.local_domain.clone())
}

/// One instance per user client. Holds the rendered snapshot; drives the seam.
/// Mirrors `ForwarderMachine`.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MailAliasesMachine {
    nest: Arc<dyn MailAliasesNest>,
    inner: Mutex<MailAliasesSnapshot>,
}

// `new` takes `Arc<dyn MailAliasesNest>` (not an FFI type), so it stays in a
// plain impl alongside the private helpers; the FFI surface lives in the
// exported impl blocks below (mirrors `forwarders.rs`).
impl MailAliasesMachine {
    pub fn new(nest: Arc<dyn MailAliasesNest>) -> Self {
        Self {
            nest,
            inner: Mutex::new(MailAliasesSnapshot::empty()),
        }
    }

    fn set_status(&self, status: AliasesStatus) {
        self.inner.lock().expect("snapshot mutex").status = status;
    }

    async fn refresh(&self) -> Result<(), DispatchError> {
        self.set_status(AliasesStatus::Loading);
        let rows = self.nest.list_account_aliases().await?;
        let aliases: Vec<AliasView> = rows.into_iter().map(AliasView::from).collect();
        let default_domain = derive_default_domain(&aliases);
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.aliases = aliases;
        snap.default_domain = default_domain;
        snap.status = AliasesStatus::Idle;
        Ok(())
    }

    async fn create(
        &self,
        kind: AliasKind,
        pattern: String,
        label: String,
        spam_threshold_override: Option<u32>,
        rate_limit_per_hour: Option<i64>,
    ) -> Result<(), DispatchError> {
        let wire_kind = kind.create_wire().ok_or_else(|| {
            DispatchError::InvalidState(format!(
                "{kind:?} aliases are not created via create (disposable mints, \
                 subaddress is resolver-only, catch-all/forwarder are admin-tier)"
            ))
        })?;
        let local_domain = self
            .inner
            .lock()
            .expect("snapshot mutex")
            .default_domain
            .clone()
            .ok_or_else(|| {
                DispatchError::InvalidState(
                    "no canonical address yet — enable mail before adding aliases".into(),
                )
            })?;

        // Pre-validate with the same pure validators the nest uses (priority #2);
        // for wildcard, strip a trailing `*` the user typed (the wire `pattern`
        // is the literal prefix incl. its trailing `-`).
        let wire_pattern = match kind {
            AliasKind::Exact => {
                validate_exact_local_part(&pattern, DEFAULT_RESERVED_LOCAL_PARTS)
                    .map_err(|e| DispatchError::InvalidState(e.to_string()))?;
                pattern
            }
            AliasKind::Wildcard => {
                let prefix = pattern.strip_suffix('*').unwrap_or(&pattern).to_string();
                validate_wildcard_prefix(&prefix, DEFAULT_RESERVED_LOCAL_PARTS)
                    .map_err(|e| DispatchError::InvalidState(e.to_string()))?;
                prefix
            }
            _ => unreachable!("create_wire() already rejected non-exact/wildcard"),
        };

        self.set_status(AliasesStatus::Working);
        let controls = AliasControls {
            label,
            spam_threshold_override,
            rate_limit_per_hour,
            rate_limit_per_day: None,
        };
        self.nest
            .create_account_alias(wire_kind.to_string(), local_domain, wire_pattern, controls)
            .await?;
        self.refresh().await
    }

    async fn generate_disposable(
        &self,
        ttl_days: Option<u32>,
        uses: Option<u32>,
        label: String,
    ) -> Result<(), DispatchError> {
        self.set_status(AliasesStatus::Working);
        let full_address = self
            .nest
            .generate_disposable_alias(ttl_days, uses, label)
            .await?;
        self.inner
            .lock()
            .expect("snapshot mutex")
            .last_minted_address = Some(full_address);
        self.refresh().await
    }

    async fn import(&self, lines: Vec<String>) -> Result<(), DispatchError> {
        self.set_status(AliasesStatus::Working);
        let outcomes = self.nest.import_account_aliases(lines).await?;
        self.inner
            .lock()
            .expect("snapshot mutex")
            .last_import_result = Some(ImportResultView::from(outcomes));
        self.refresh().await
    }

    async fn update(
        &self,
        alias_id_hex: String,
        pattern: String,
        label: String,
        spam_threshold_override: Option<u32>,
        rate_limit_per_hour: Option<i64>,
    ) -> Result<(), DispatchError> {
        let alias_id = decode_alias_id(&alias_id_hex)?;
        // Preserve the per-day cap the seed add-sheet doesn't expose (full
        // overwrite would otherwise drop it).
        let rate_limit_per_day = self
            .inner
            .lock()
            .expect("snapshot mutex")
            .aliases
            .iter()
            .find(|a| a.alias_id_hex == alias_id_hex)
            .and_then(|a| a.rate_limit_per_day);

        self.set_status(AliasesStatus::Working);
        let controls = AliasControls {
            label,
            spam_threshold_override,
            rate_limit_per_hour,
            rate_limit_per_day,
        };
        self.nest
            .update_account_alias(alias_id, pattern, controls)
            .await?;
        self.refresh().await
    }

    async fn revoke(&self, alias_id_hex: String) -> Result<(), DispatchError> {
        let alias_id = decode_alias_id(&alias_id_hex)?;
        self.set_status(AliasesStatus::Working);
        self.nest.revoke_account_alias(alias_id).await?;
        self.refresh().await
    }

    async fn enable(&self, alias_id_hex: String) -> Result<(), DispatchError> {
        let alias_id = decode_alias_id(&alias_id_hex)?;
        self.set_status(AliasesStatus::Working);
        self.nest.enable_account_alias(alias_id).await?;
        self.refresh().await
    }

    async fn delete(&self, alias_id_hex: String) -> Result<(), DispatchError> {
        let alias_id = decode_alias_id(&alias_id_hex)?;
        self.set_status(AliasesStatus::Working);
        self.nest.delete_account_alias(alias_id).await?;
        self.refresh().await
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl MailAliasesMachine {
    pub fn snapshot(&self) -> MailAliasesSnapshot {
        fauna_core::clone_locked(&self.inner, |s| s)
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl MailAliasesMachine {
    /// Initial page load.
    pub async fn hydrate(&self) -> Result<(), DispatchError> {
        self.refresh().await
    }

    pub async fn dispatch(&self, action: MailAliasesAction) -> Result<(), DispatchError> {
        // Clear any prior error + minted-address before the new action runs.
        {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            snap.error = None;
            snap.last_minted_address = None;
            snap.last_import_result = None;
        }
        let result = match action {
            MailAliasesAction::Refresh => self.refresh().await,
            MailAliasesAction::Create {
                kind,
                pattern,
                label,
                spam_threshold_override,
                rate_limit_per_hour,
            } => {
                self.create(
                    kind,
                    pattern,
                    label,
                    spam_threshold_override,
                    rate_limit_per_hour,
                )
                .await
            }
            MailAliasesAction::GenerateDisposable {
                ttl_days,
                uses,
                label,
            } => self.generate_disposable(ttl_days, uses, label).await,
            MailAliasesAction::Update {
                alias_id_hex,
                pattern,
                label,
                spam_threshold_override,
                rate_limit_per_hour,
            } => {
                self.update(
                    alias_id_hex,
                    pattern,
                    label,
                    spam_threshold_override,
                    rate_limit_per_hour,
                )
                .await
            }
            MailAliasesAction::Revoke { alias_id_hex } => self.revoke(alias_id_hex).await,
            MailAliasesAction::Enable { alias_id_hex } => self.enable(alias_id_hex).await,
            MailAliasesAction::Delete { alias_id_hex } => self.delete(alias_id_hex).await,
            MailAliasesAction::Import { lines } => self.import(lines).await,
        };
        if let Err(ref e) = result {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            crate::state::set_snapshot_error(&mut snap.error, e.to_string());
            snap.status = AliasesStatus::Idle;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::ByteBuf;
    use std::sync::Mutex as StdMutex;

    /// In-memory nest modelling the user's `account_aliases`: `list` returns the
    /// rows; `create` inserts (refusing a `(local_domain, pattern, kind)`
    /// collision); `update`/`revoke`/`delete` mutate by id; `generate` mints a
    /// disposable row + returns its full address.
    #[derive(Default)]
    struct FakeNest {
        rows: StdMutex<Vec<AliasRow>>,
        next_id: StdMutex<u8>,
    }

    fn row(id: u8, domain: &str, kind: &str, pattern: &str) -> AliasRow {
        AliasRow {
            alias_id: ByteBuf::from(vec![id; 16]),
            actor_id: ByteBuf::from(vec![0u8; 32]),
            local_domain: domain.into(),
            kind: kind.into(),
            pattern: pattern.into(),
            created_at: 1_700_000_000_000,
            ..Default::default()
        }
    }

    /// Seed helper for the import tests: an existing `Exact` alias at
    /// `<local>@<domain>`.
    fn exact_row(local: &str, domain: &str) -> AliasRow {
        row(0x01, domain, ALIAS_KIND_EXACT, local)
    }

    #[async_trait]
    impl MailAliasesNest for FakeNest {
        async fn list_account_aliases(&self) -> Result<Vec<AliasRow>, NestError> {
            Ok(self.rows.lock().unwrap().clone())
        }

        async fn create_account_alias(
            &self,
            kind: String,
            local_domain: String,
            pattern: String,
            controls: AliasControls,
        ) -> Result<(), NestError> {
            let mut rows = self.rows.lock().unwrap();
            if rows
                .iter()
                .any(|r| r.local_domain == local_domain && r.pattern == pattern && r.kind == kind)
            {
                return Err(NestError::Rejected("conflicts_with_existing_alias".into()));
            }
            let mut id = self.next_id.lock().unwrap();
            *id += 1;
            let mut r = row(*id, &local_domain, &kind, &pattern);
            r.label = controls.label;
            r.spam_threshold_override = controls.spam_threshold_override;
            r.rate_limit_per_hour = controls.rate_limit_per_hour;
            r.rate_limit_per_day = controls.rate_limit_per_day;
            rows.push(r);
            Ok(())
        }

        async fn update_account_alias(
            &self,
            alias_id: Vec<u8>,
            pattern: String,
            controls: AliasControls,
        ) -> Result<(), NestError> {
            let mut rows = self.rows.lock().unwrap();
            let r = rows
                .iter_mut()
                .find(|r| r.alias_id.as_ref() == alias_id.as_slice())
                .ok_or_else(|| NestError::Rejected("fauna.bridges.not_found".into()))?;
            r.pattern = pattern;
            r.label = controls.label;
            r.spam_threshold_override = controls.spam_threshold_override;
            r.rate_limit_per_hour = controls.rate_limit_per_hour;
            r.rate_limit_per_day = controls.rate_limit_per_day;
            Ok(())
        }

        async fn revoke_account_alias(&self, alias_id: Vec<u8>) -> Result<(), NestError> {
            let mut rows = self.rows.lock().unwrap();
            let r = rows
                .iter_mut()
                .find(|r| r.alias_id.as_ref() == alias_id.as_slice())
                .ok_or_else(|| NestError::Rejected("fauna.bridges.not_found".into()))?;
            r.disabled = true;
            Ok(())
        }

        async fn enable_account_alias(&self, alias_id: Vec<u8>) -> Result<(), NestError> {
            let mut rows = self.rows.lock().unwrap();
            let r = rows
                .iter_mut()
                .find(|r| r.alias_id.as_ref() == alias_id.as_slice())
                .ok_or_else(|| NestError::Rejected("fauna.bridges.not_found".into()))?;
            r.disabled = false;
            Ok(())
        }

        async fn delete_account_alias(&self, alias_id: Vec<u8>) -> Result<(), NestError> {
            let mut rows = self.rows.lock().unwrap();
            let before = rows.len();
            rows.retain(|r| r.alias_id.as_ref() != alias_id.as_slice());
            if rows.len() == before {
                return Err(NestError::Rejected("fauna.bridges.not_found".into()));
            }
            Ok(())
        }

        async fn generate_disposable_alias(
            &self,
            _ttl_days: Option<u32>,
            _uses: Option<u32>,
            label: String,
        ) -> Result<String, NestError> {
            let mut rows = self.rows.lock().unwrap();
            // Mint needs the canonical exact alias to derive handle + domain.
            let canonical = rows
                .iter()
                .find(|r| r.kind == ALIAS_KIND_EXACT)
                .cloned()
                .ok_or_else(|| NestError::Rejected("fauna.bridges.no_canonical_address".into()))?;
            let mut id = self.next_id.lock().unwrap();
            *id += 1;
            let token = "a2b3c4";
            let mut r = row(*id, &canonical.local_domain, ALIAS_KIND_DISPOSABLE, token);
            r.label = label;
            r.uses_remaining = Some(1);
            r.expires_at = Some(1_700_002_592_000);
            rows.push(r);
            Ok(format!(
                "{}-temp-{token}@{}",
                canonical.pattern, canonical.local_domain
            ))
        }

        async fn import_account_aliases(
            &self,
            lines: Vec<String>,
        ) -> Result<Vec<ImportAliasOutcome>, NestError> {
            let mut rows = self.rows.lock().unwrap();
            let mut id = self.next_id.lock().unwrap();
            let mut outcomes = Vec::new();
            for (line_index, line) in lines.iter().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let parsed = trimmed
                    .split_once('@')
                    .filter(|(local, domain)| !local.is_empty() && !domain.is_empty());
                let Some((local, domain)) = parsed else {
                    outcomes.push(ImportAliasOutcome {
                        line_index: line_index as u32,
                        address: trimmed.to_string(),
                        status: ImportAliasStatus::Invalid,
                        reason: Some("not a valid email address".into()),
                    });
                    continue;
                };
                if rows.iter().any(|r| {
                    r.local_domain == domain && r.pattern == local && r.kind == ALIAS_KIND_EXACT
                }) {
                    outcomes.push(ImportAliasOutcome {
                        line_index: line_index as u32,
                        address: trimmed.to_string(),
                        status: ImportAliasStatus::SkippedDuplicate,
                        reason: Some("already exists".into()),
                    });
                    continue;
                }
                *id += 1;
                rows.push(row(*id, domain, ALIAS_KIND_EXACT, local));
                outcomes.push(ImportAliasOutcome {
                    line_index: line_index as u32,
                    address: trimmed.to_string(),
                    status: ImportAliasStatus::Created,
                    reason: None,
                });
            }
            Ok(outcomes)
        }
    }

    fn machine_with(rows: Vec<AliasRow>) -> MailAliasesMachine {
        MailAliasesMachine::new(Arc::new(FakeNest {
            rows: StdMutex::new(rows),
            next_id: StdMutex::new(0x10),
        }))
    }

    #[tokio::test]
    async fn refresh_projects_rows_and_default_domain() {
        let m = machine_with(vec![
            row(0x01, "example.com", "exact", "bob"),
            row(0x02, "example.com", "wildcard_prefix", "bob-"),
        ]);
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.aliases.len(), 2);
        assert_eq!(snap.aliases[0].kind, AliasKind::Exact);
        assert_eq!(snap.aliases[0].address, "bob@example.com");
        assert_eq!(snap.aliases[1].kind, AliasKind::Wildcard);
        assert_eq!(snap.aliases[1].address, "bob-*@example.com");
        // Default domain comes from the (only) exact alias.
        assert_eq!(snap.default_domain.as_deref(), Some("example.com"));
        assert_eq!(snap.status, AliasesStatus::Idle);
        assert!(snap.error.is_none());
    }

    /// `default_domain` must come from the CANONICAL exact alias, never from
    /// whichever exact alias happens to sort first by its `alias_id_hex`
    /// string. `alias_id` is a random UUIDv4 (`bins/fauna-nest/src/db/
    /// mail_aliases.rs`) — not time-sortable — so a min-by-hex heuristic picks
    /// an effectively random domain once a user has exact aliases on more
    /// than one domain (own-domain handle + a locally-registered
    /// `bob.smith@example.com`-style extra). Reproduces the reported bug:
    /// the canonical row here (`0x02`) has a LEXICOGRAPHICALLY LARGER id hex
    /// than the non-canonical row (`0x01`), so a hex-min heuristic would pick
    /// the wrong domain even though the canonical row lists second.
    #[tokio::test]
    async fn default_domain_follows_the_canonical_row_not_the_smallest_id_hex() {
        let mut wrong = row(0x01, "wrong.example", "exact", "bob");
        wrong.is_canonical = false;
        let mut canonical = row(0x02, "canon.example", "exact", "bob");
        canonical.is_canonical = true;
        let m = machine_with(vec![wrong, canonical]);
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(
            snap.default_domain.as_deref(),
            Some("canon.example"),
            "default_domain must follow the canonical row, not the row whose \
             alias_id_hex happens to sort first"
        );
    }

    /// The wire `AliasRow.is_canonical` flag projects straight onto the
    /// `AliasView` the UI renders read-only (`mail-aliases.md` § Aliases UX).
    #[test]
    fn alias_view_projects_is_canonical() {
        let mut canonical = row(0x01, "example.com", "exact", "bob");
        canonical.is_canonical = true;
        let ordinary = row(0x02, "example.com", "exact", "bob.smith");
        assert!(AliasView::from(canonical).is_canonical);
        assert!(!AliasView::from(ordinary).is_canonical);
    }

    #[tokio::test]
    async fn create_exact_validates_and_relists() {
        let m = machine_with(vec![row(0x01, "example.com", "exact", "bob")]);
        m.hydrate().await.unwrap();
        m.dispatch(MailAliasesAction::Create {
            kind: AliasKind::Exact,
            pattern: "bob.smith".into(),
            label: "work".into(),
            spam_threshold_override: Some(8),
            rate_limit_per_hour: None,
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.aliases.len(), 2);
        let added = snap
            .aliases
            .iter()
            .find(|a| a.pattern == "bob.smith")
            .unwrap();
        assert_eq!(added.address, "bob.smith@example.com");
        assert_eq!(added.label, "work");
        assert_eq!(added.spam_threshold_override, Some(8));
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn create_reserved_local_part_rejected_client_side() {
        let m = machine_with(vec![row(0x01, "example.com", "exact", "bob")]);
        m.hydrate().await.unwrap();
        // `postmaster` is reserved — the shared validator refuses before any RPC.
        let err = m
            .dispatch(MailAliasesAction::Create {
                kind: AliasKind::Exact,
                pattern: "postmaster".into(),
                label: String::new(),
                spam_threshold_override: None,
                rate_limit_per_hour: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::InvalidState(_)), "got {err:?}");
        // List untouched — the create never reached the (fake) nest.
        assert_eq!(m.snapshot().aliases.len(), 1);
        assert!(m.snapshot().error.is_some());
    }

    #[tokio::test]
    async fn create_wildcard_strips_star_and_validates_lead() {
        let m = machine_with(vec![row(0x01, "example.com", "exact", "bob")]);
        m.hydrate().await.unwrap();
        // User typed `news-*`; the machine strips `*`, validates the `news-`
        // prefix, and stores the literal prefix.
        m.dispatch(MailAliasesAction::Create {
            kind: AliasKind::Wildcard,
            pattern: "news-*".into(),
            label: String::new(),
            spam_threshold_override: None,
            rate_limit_per_hour: None,
        })
        .await
        .unwrap();
        let added = m
            .snapshot()
            .aliases
            .into_iter()
            .find(|a| a.kind == AliasKind::Wildcard)
            .unwrap();
        assert_eq!(added.pattern, "news-");
        assert_eq!(added.address, "news-*@example.com");
    }

    #[tokio::test]
    async fn create_wildcard_too_short_rejected_client_side() {
        let m = machine_with(vec![row(0x01, "example.com", "exact", "bob")]);
        m.hydrate().await.unwrap();
        // `a-` has only one lead char (< MIN_WILDCARD_PREFIX_LEAD).
        let err = m
            .dispatch(MailAliasesAction::Create {
                kind: AliasKind::Wildcard,
                pattern: "a-".into(),
                label: String::new(),
                spam_threshold_override: None,
                rate_limit_per_hour: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::InvalidState(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn create_without_canonical_address_is_unavailable() {
        // No exact alias → no default_domain → create refused before any RPC.
        let m = machine_with(vec![]);
        m.hydrate().await.unwrap();
        assert!(m.snapshot().default_domain.is_none());
        let err = m
            .dispatch(MailAliasesAction::Create {
                kind: AliasKind::Exact,
                pattern: "bob".into(),
                label: String::new(),
                spam_threshold_override: None,
                rate_limit_per_hour: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::InvalidState(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn create_collision_surfaces_error_and_keeps_list() {
        let m = machine_with(vec![
            row(0x01, "example.com", "exact", "bob"),
            row(0x02, "example.com", "exact", "bob.smith"),
        ]);
        m.hydrate().await.unwrap();
        let err = m
            .dispatch(MailAliasesAction::Create {
                kind: AliasKind::Exact,
                pattern: "bob.smith".into(),
                label: String::new(),
                spam_threshold_override: None,
                rate_limit_per_hour: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::Nest(_)), "got {err:?}");
        let snap = m.snapshot();
        assert!(
            snap.error
                .as_deref()
                .unwrap()
                .contains("conflicts_with_existing_alias"),
            "error: {:?}",
            snap.error
        );
        assert_eq!(snap.aliases.len(), 2, "list untouched");
        assert_eq!(snap.status, AliasesStatus::Idle);
    }

    #[tokio::test]
    async fn generate_disposable_sets_minted_address_and_adds_row() {
        let m = machine_with(vec![row(0x01, "example.com", "exact", "bob")]);
        m.hydrate().await.unwrap();
        m.dispatch(MailAliasesAction::GenerateDisposable {
            ttl_days: None,
            uses: None,
            label: "amazon".into(),
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        assert_eq!(
            snap.last_minted_address.as_deref(),
            Some("bob-temp-a2b3c4@example.com")
        );
        let disp = snap
            .aliases
            .iter()
            .find(|a| a.kind == AliasKind::Disposable)
            .unwrap();
        assert_eq!(disp.label, "amazon");
        assert_eq!(disp.uses_remaining, Some(1));
        assert!(disp.address.contains("-temp-"));
    }

    #[tokio::test]
    async fn revoke_then_enable_round_trips_disabled() {
        // The reported disable-trap: disabling an alias must be reversible.
        let m = machine_with(vec![
            row(0x01, "example.com", "exact", "bob"),
            row(0x02, "example.com", "wildcard_prefix", "bob-"),
        ]);
        m.hydrate().await.unwrap();
        let id_hex = m.snapshot().aliases[1].alias_id_hex.clone();
        assert!(!m.snapshot().aliases[1].disabled);

        m.dispatch(MailAliasesAction::Revoke {
            alias_id_hex: id_hex.clone(),
        })
        .await
        .unwrap();
        assert!(m.snapshot().aliases[1].disabled, "revoke disables");

        m.dispatch(MailAliasesAction::Enable {
            alias_id_hex: id_hex,
        })
        .await
        .unwrap();
        assert!(
            !m.snapshot().aliases[1].disabled,
            "enable re-enables — disable is not a one-way trap"
        );
    }

    #[tokio::test]
    async fn next_dispatch_clears_minted_address() {
        let m = machine_with(vec![row(0x01, "example.com", "exact", "bob")]);
        m.hydrate().await.unwrap();
        m.dispatch(MailAliasesAction::GenerateDisposable {
            ttl_days: None,
            uses: None,
            label: String::new(),
        })
        .await
        .unwrap();
        assert!(m.snapshot().last_minted_address.is_some());
        m.dispatch(MailAliasesAction::Refresh).await.unwrap();
        assert!(m.snapshot().last_minted_address.is_none());
    }

    #[tokio::test]
    async fn update_preserves_unexposed_per_day_cap() {
        let mut seed = row(0x05, "example.com", "exact", "bob");
        seed.rate_limit_per_day = Some(500);
        let m = machine_with(vec![seed]);
        m.hydrate().await.unwrap();
        let id_hex = m.snapshot().aliases[0].alias_id_hex.clone();
        m.dispatch(MailAliasesAction::Update {
            alias_id_hex: id_hex,
            pattern: "bob".into(),
            label: "renamed".into(),
            spam_threshold_override: Some(3),
            rate_limit_per_hour: Some(20),
        })
        .await
        .unwrap();
        let row = &m.snapshot().aliases[0];
        assert_eq!(row.label, "renamed");
        assert_eq!(row.spam_threshold_override, Some(3));
        assert_eq!(row.rate_limit_per_hour, Some(20));
        // The per-day cap the add-sheet doesn't expose survives the overwrite.
        assert_eq!(row.rate_limit_per_day, Some(500));
    }

    #[tokio::test]
    async fn revoke_sets_disabled_and_keeps_row() {
        let m = machine_with(vec![row(0x07, "example.com", "exact", "bob")]);
        m.hydrate().await.unwrap();
        let id_hex = m.snapshot().aliases[0].alias_id_hex.clone();
        m.dispatch(MailAliasesAction::Revoke {
            alias_id_hex: id_hex,
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.aliases.len(), 1, "revoke preserves the row");
        assert!(snap.aliases[0].disabled);
    }

    #[tokio::test]
    async fn delete_removes_row() {
        let m = machine_with(vec![row(0x09, "example.com", "exact", "bob")]);
        m.hydrate().await.unwrap();
        let id_hex = m.snapshot().aliases[0].alias_id_hex.clone();
        m.dispatch(MailAliasesAction::Delete {
            alias_id_hex: id_hex,
        })
        .await
        .unwrap();
        assert!(m.snapshot().aliases.is_empty());
    }

    #[tokio::test]
    async fn delete_malformed_hex_surfaces_wrap_without_calling_nest() {
        let m = machine_with(vec![row(0x01, "example.com", "exact", "bob")]);
        m.hydrate().await.unwrap();
        let err = m
            .dispatch(MailAliasesAction::Delete {
                alias_id_hex: "zz-not-hex".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::Wrap(_)), "got {err:?}");
        assert_eq!(m.snapshot().aliases.len(), 1, "row untouched");
    }

    #[tokio::test]
    async fn import_reports_outcomes_and_relists() {
        // Seed one existing exact alias so a re-paste of it is SkippedDuplicate.
        let m = machine_with(vec![exact_row("me-netflix", "example.com")]);
        m.hydrate().await.unwrap();
        m.dispatch(MailAliasesAction::Import {
            lines: vec![
                "me-amazon@example.com".into(),  // Created
                "me-netflix@example.com".into(), // SkippedDuplicate (exists)
                "not-an-address".into(),         // Invalid
            ],
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        let res = snap.last_import_result.expect("import result set");
        assert_eq!(res.created, 1);
        assert_eq!(res.skipped_duplicate, 1);
        assert_eq!(res.invalid, 1);
        assert_eq!(res.outcomes.len(), 3);
        // The list refreshed to include the new alias.
        assert!(snap.aliases.iter().any(|a| a.pattern == "me-amazon"));
        assert!(snap.error.is_none());
    }

    // ── alias kind badge ────────────────────────────────────────────────

    #[test]
    fn alias_kind_badge_maps_every_variant() {
        for (kind, key) in [
            (AliasKind::Exact, "mail_aliases.kind_exact"),
            (AliasKind::Subaddress, "mail_aliases.kind_subaddress"),
            (AliasKind::Wildcard, "mail_aliases.kind_wildcard"),
            (AliasKind::Disposable, "mail_aliases.kind_disposable"),
            (AliasKind::Catchall, "mail_aliases.kind_catchall"),
            (AliasKind::Forwarder, "mail_aliases.kind_forwarder"),
            (AliasKind::Other, "mail_aliases.kind_other"),
        ] {
            assert_eq!(alias_kind_badge(kind).key, key, "{kind:?}");
        }
    }

    // ── alias hits label ────────────────────────────────────────────────

    #[test]
    fn alias_hits_label_maps_count_and_optional_date() {
        // No last-hit date → bare count key, only the `count` arg.
        let t = alias_hits_label(5, None);
        assert_eq!(t.key, "mail_aliases.hits");
        assert_eq!(t.args.get("count"), Some(&"5".to_string()));
        assert!(!t.args.contains_key("date"));

        // With a (client-formatted, tz-local) date → the with-last key + both args.
        let t = alias_hits_label(3, Some("2026-06-28".to_string()));
        assert_eq!(t.key, "mail_aliases.hits_with_last");
        assert_eq!(t.args.get("count"), Some(&"3".to_string()));
        assert_eq!(t.args.get("date"), Some(&"2026-06-28".to_string()));

        // Resolving the `en` templates reproduces the literal text the four
        // apps each used to hard-code (behavior-preserving for `en`).
        let lookup = |k: &str| match k {
            "mail_aliases.hits" => Some("{count} hits"),
            "mail_aliases.hits_with_last" => Some("{count} hits · last {date}"),
            _ => None,
        };
        assert_eq!(alias_hits_label(5, None).resolve(lookup), "5 hits");
        assert_eq!(
            alias_hits_label(3, Some("2026-06-28".to_string())).resolve(lookup),
            "3 hits · last 2026-06-28"
        );
    }

    /// A line status a newer nest added (`Unknown`) reads as not created: it
    /// counts as invalid and renders as an invalid line, with the nest's reason.
    #[test]
    fn an_unknown_import_status_counts_as_not_created_and_keeps_its_reason() {
        let result = ImportResultView::from(vec![
            ImportAliasOutcome {
                line_index: 0,
                address: "ok@example.com".into(),
                status: ImportAliasStatus::Created,
                reason: None,
            },
            ImportAliasOutcome {
                line_index: 1,
                address: "new@example.com".into(),
                status: ImportAliasStatus::Unknown,
                reason: Some("needs a newer version".into()),
            },
        ]);
        assert_eq!(
            (result.created, result.skipped_duplicate, result.invalid),
            (1, 0, 1)
        );
        assert_eq!(result.outcomes[1].status, ImportAliasStatusView::Invalid);
        assert_eq!(
            result.outcomes[1].reason.as_deref(),
            Some("needs a newer version")
        );
    }
}
