//! Trust facet (Settings → Nests) — the FFI/JS-facing snapshot types + the
//! seams the machine needs to mint / renew / revoke content-processing grants
//! and fold the signed grant-event log per nest.
//!
//! Design authority: `docs/goal/ui/nests.md` (§ Where logic lives, § Data
//! shape) — the Nests-page machine *extends* `LinkedNestsMachine` (this crate's
//! `lib.rs`) with a per-nest trust facet. The pure computation lives in
//! `fauna-client-capabilities` (`mint_grant`, `grant_log`, `view_model`); this
//! module owns only (a) the boundary-facing projection of those pure results
//! into UniFFI/serde snapshot rows, and (b) the platform seams (config store,
//! signer, clock/randomness) the machine drives — mirroring the
//! `MailSettingsMachine` seam layout (`fauna-client-mail-settings::machine`).
//!
//! **Why pair-local FFI rows** (not the pure `view_model` types directly): the
//! snapshot crosses the UniFFI (`uniffi::Record`) + wasm (`serde_wasm_bindgen`)
//! boundary, so its types need those derives. Keeping them here — the crate
//! that owns the `uniffi::Object` machine — leaves `fauna-core`/`fauna-mls`/
//! `fauna-client-capabilities` free of UniFFI scaffolding (the `GrantEventScope`
//! / `GrantEventKind` / `GrantView` types stay pure), exactly as
//! `LinkedNestRow` is the FFI projection of the wire `PairingRow` and
//! `SpamModelClientWrite` is mail-settings' FFI projection of its pure outcome.

use std::sync::Arc;

use fauna_core::grant_event::{GrantEventKind, GrantEventScope};
use fauna_core::localized::LocalizedText;
use fauna_protocol::MaybeSendSync;
use serde::{Deserialize, Serialize};

use fauna_client_capabilities::view_model::{
    self as view_model, GrantLiveness, GrantView, HistoryEntry, PrincipalFolder,
};

// The grant-event log read/write seam is the **shared**
// `fauna_client_config::SuccessionLedgerStore` (priority #4: one seam across
// dns/mail/pair). Its impl owns the `grant_events`-union merge, so the machine
// treats it as opaque.
pub use fauna_client_config::{BackupStateStore, SuccessionLedgerStore};

// ── FFI-facing snapshot rows ────────────────────────────────────────

/// Which lens a nest row currently shows in the trust facet — the per-row UI
/// state a `SetLens` action flips (`nests.md:29` — a *per-row* facet, not a
/// page-level toggle). `Now` = the current-grants projection; `History` = the
/// raw event timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TrustLens {
    /// Current grants (folded) — the default lens.
    #[default]
    Now,
    /// The raw mint/renew/revoke timeline.
    History,
}

/// One declared scope tuple, FFI-projected from
/// [`fauna_core::grant_event::GrantEventScope`]. [`scope_label`] maps
/// `(class, kind, tier)` to its `LocalizedText` "Trusted to read: Mail,
/// Calendar" key; each app resolves it through its own i18n runtime (one
/// shared mapping, no baked-in strings here).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TrustScope {
    pub class: String,
    pub kind: Option<String>,
    pub tier: Option<String>,
}

/// The folder a third-party principal's folder read grant covers,
/// FFI-projected from [`PrincipalFolder`] — what [`scope_label_in_folder`]
/// names in place of the bare `folder` kind (`webdav-server.md` § Key model →
/// *A principal's read* rule (1)).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TrustFolder {
    /// The owner's set of that name.
    Named { name: String },
    /// A set the owner no longer has.
    Deleted,
}

impl From<PrincipalFolder> for TrustFolder {
    fn from(folder: PrincipalFolder) -> Self {
        match folder {
            PrincipalFolder::Named(name) => TrustFolder::Named { name },
            PrincipalFolder::Deleted => TrustFolder::Deleted,
        }
    }
}

/// A grant's liveness relative to `now`, FFI-projected from
/// [`fauna_client_capabilities::view_model::GrantLiveness`] — the state
/// `nest-trust-grant-status` renders (`nests.md` § Expiry / renewal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TrustLiveness {
    Active,
    ExpiringSoon,
    Expired,
    AutoRenewing,
}

/// How long a new grant lasts — the `nest-trust-mint-duration-select` choice,
/// FFI projection of [`fauna_client_capabilities::GrantDuration`]
/// (`nests.md` § Expiry / renewal → *Duration and blessing*). The two windows
/// are hard-coded constants; this is only which one the user picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TrustGrantDuration {
    /// A few hours ([`fauna_client_capabilities::ONE_OFF_GRANT_WINDOW_SECS`]);
    /// never auto-renewed. The default on an un-blessed nest.
    #[default]
    OneOff,
    /// ~90 days ([`fauna_client_capabilities::DEFAULT_GRANT_WINDOW_SECS`]);
    /// auto-renewed on a blessed nest, where it is the default.
    Standard,
}

impl From<TrustGrantDuration> for fauna_client_capabilities::GrantDuration {
    fn from(d: TrustGrantDuration) -> Self {
        match d {
            TrustGrantDuration::OneOff => Self::OneOff,
            TrustGrantDuration::Standard => Self::Standard,
        }
    }
}

impl From<fauna_client_capabilities::GrantDuration> for TrustGrantDuration {
    fn from(d: fauna_client_capabilities::GrantDuration) -> Self {
        match d {
            fauna_client_capabilities::GrantDuration::OneOff => Self::OneOff,
            fauna_client_capabilities::GrantDuration::Standard => Self::Standard,
        }
    }
}

/// The durations `nest-trust-mint-duration-select` offers, in display order —
/// one list every app's picker renders, so none can offer a third.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn mint_duration_options() -> Vec<TrustGrantDuration> {
    vec![TrustGrantDuration::OneOff, TrustGrantDuration::Standard]
}

/// The auto-renew loop's cadence in seconds
/// (`fauna_client_capabilities::view_model::AUTO_RENEW_CHECK_SECS`) — the one
/// constant every desktop shell's app-level `AutoRenew` timer reads, so none
/// hard-codes its own (wasm exposes the same value as `autoRenewCheckSecs`).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn auto_renew_check_secs() -> u64 {
    fauna_client_capabilities::view_model::AUTO_RENEW_CHECK_SECS
}

/// Localized label for a [`TrustGrantDuration`] option.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn duration_label(d: TrustGrantDuration) -> LocalizedText {
    LocalizedText::key(match d {
        TrustGrantDuration::OneOff => "nests.mint_duration_one_off",
        TrustGrantDuration::Standard => "nests.mint_duration_standard",
    })
}

/// One current grant row for the Now lens (`nest-trust-grant-item`), FFI
/// projection of [`GrantView`]. `grant_id` is the handle a `Renew`/`Revoke`
/// action names; `holder` is the content-processor X25519 pubkey it is sealed
/// to. An empty grant list on a nest is the `nest-trust-empty` state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TrustGrantRow {
    pub grant_id: Vec<u8>,
    pub holder: Vec<u8>,
    pub scope: Vec<TrustScope>,
    /// The grant's `lasts-until` (epoch seconds; `i64` to match the sibling
    /// timestamp fields on `LinkedNestRow`).
    pub lasts_until: i64,
    pub liveness: TrustLiveness,
    /// Whether this grant is carried across a succession and **not yet
    /// adjudicated** by the owner — the post-succession review mark
    /// (`succession-aftermath.md` § Adjudicating what the aftermath carries
    /// across). The shell renders `nest-trust-grant-unattested-mark` +
    /// `nest-trust-grant-keep-button` only while it is true, and nothing at
    /// all otherwise.
    ///
    /// It lives on the row rather than being cross-referenced per shell
    /// because the two halves come from different planes — the grant from the
    /// live nest roster, the mark from the succession ledger — and a shell that joined
    /// them itself would be seven chances to get the join wrong. Here it is
    /// joined once, where both are already in hand.
    pub unattested: bool,
    /// The folder a folder read grant covers — today the web-serve paywall
    /// grant, whose signed event carries no set name, resolved through
    /// [`TrustFolderNames`] (`webdav-server.md` § Key model → *A principal's
    /// read* rule (1) → *The generation*). The shell labels the scope through
    /// [`scope_label_in_folder`]; `None` for any other grant, and on a machine
    /// built without the set-name seam.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub folder: Option<TrustFolder>,
}

/// Which lifecycle transition a [`TrustHistoryRow`] describes — FFI projection
/// of [`fauna_core::grant_event::GrantEventKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TrustEventKind {
    Mint,
    Renew,
    Revoke,
}

/// One grant-event row for the History lens (`nest-trust-history-item`), FFI
/// projection of [`HistoryEntry`] — self-describing ("Minted / Renewed /
/// Revoked ‹scope› · ‹when›"; empty `scope` + zeroed window on a `Revoke`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TrustHistoryRow {
    pub grant_id: Vec<u8>,
    pub holder: Vec<u8>,
    pub kind: TrustEventKind,
    pub scope: Vec<TrustScope>,
    pub window_start: i64,
    pub window_end: i64,
    pub at: i64,
    /// [`TrustGrantRow::folder`] for the event's grant. Matched by id, so a
    /// `Revoke` (empty `scope`) of a named folder's grant is named too.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub folder: Option<TrustFolder>,
}

/// Which use case a mint-picker option represents — FFI projection of
/// [`fauna_client_capabilities::view_model::MintUseCase`]; [`mint_option_label`]
/// maps it to its `LocalizedText` option label ("Read and filter my mail" /
/// "Read my calendar" / "Serve paywalled posts — ‹tier›").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TrustMintUseCase {
    Mail,
    Calendar,
    PaywalledPosts,
}

/// One option in the trust facet's mint picker (`nest-trust-mint-scope-select`),
/// FFI projection of [`view_model::MintOptionModel`] — a use case pre-resolved
/// to the scope it mints and the holder(s) that can take it (`nests.md` § Mint,
/// scope-first design ratified 2026-07-13). Exactly one `holder_candidates`
/// entry ⇒ the shell mints to it directly; more ⇒ it renders the conditional
/// `nest-trust-mint-holder-select`. Confirming dispatches
/// [`crate::LinkedNestsAction::Mint`] with the option's `scope` and the
/// (derived or picked) candidate as `holder_bridge_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TrustMintOption {
    pub use_case: TrustMintUseCase,
    /// The tier a `PaywalledPosts` option serves; `None` otherwise.
    pub tier: Option<String>,
    pub scope: Vec<TrustScope>,
    /// `bridge_id`s this option's grant can target, in roster order; non-empty.
    pub holder_candidates: Vec<String>,
}

// ── backup trust rows (`nest-trust-backup-item`) ────────────────────
//
// The FFI projection of `fauna_client_backup::trust`'s pure rows, same "why
// pair-local FFI rows" reasoning as the grant rows above: the snapshot crosses
// UniFFI + wasm, so its types need those derives, and keeping them here leaves
// `fauna-client-backup` free of UniFFI scaffolding.

/// Which backup power a [`TrustBackupRow`] describes — the discriminator the
/// shell turns into `nest-trust-backup-scope` copy ("Backs up your messages…" /
/// "Writes your backups to ‹destination›"). FFI projection of
/// [`fauna_client_backup::trust::BackupTrustKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TrustBackupKind {
    /// The owner→source-nest `NestBackupKey` seal grant.
    Seal,
    /// The source nest's writer authorization at one backup destination.
    Writer,
}

/// A backup trust row's state (`nest-trust-backup-status`) — FFI projection of
/// [`fauna_client_backup::trust::BackupTrustStatus`]. `Unreachable` ("we could
/// not ask the destination") stays distinct from `Missing` ("the destination
/// answered and holds no such trust"); collapsing them would let a flaky network
/// read as a revoked backup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TrustBackupStatus {
    Active,
    Unreachable,
    Missing,
}

/// One backup trust row on the home nest's trust facet (`nest-trust-backup-item`,
/// `nests.md` § Trust facet — backup rows). Rendered in the **Now** lens after
/// the content-processing grant rows.
///
/// Deliberately carries no `lasts_until` / liveness / History twin: both backup
/// grants are standing-until-revoked live nest reads, not folds of the signed
/// grant-event log (`nests.md:99`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TrustBackupRow {
    pub kind: TrustBackupKind,
    pub status: TrustBackupStatus,
    /// `BackupDestination::destination_id` on a [`TrustBackupKind::Writer`] row —
    /// what a `RevokeBackupWriter` action names. Empty on the seal row.
    pub destination_id: String,
    /// The destination's user-facing label, resolved by the shared
    /// [`fauna_core::format::backup_destination_label`] fallback. Empty on the
    /// seal row.
    pub destination_label: String,
    /// The writer grant's `granted_at` (epoch seconds) — `nest-trust-backup-since`.
    /// `None` on the seal row, which carries no timestamp on the wire; the shell
    /// renders that leaf empty.
    pub since: Option<i64>,
}

pub(crate) fn project_backup_row(r: fauna_client_backup::trust::BackupTrustRow) -> TrustBackupRow {
    use fauna_client_backup::trust::{BackupTrustKind, BackupTrustStatus};
    let (kind, destination_id, destination_label) = match r.kind {
        BackupTrustKind::Seal => (TrustBackupKind::Seal, String::new(), String::new()),
        BackupTrustKind::Writer {
            destination_id,
            destination_label,
            ..
        } => (TrustBackupKind::Writer, destination_id, destination_label),
    };
    TrustBackupRow {
        kind,
        status: match r.status {
            BackupTrustStatus::Active => TrustBackupStatus::Active,
            BackupTrustStatus::Unreachable => TrustBackupStatus::Unreachable,
            BackupTrustStatus::Missing => TrustBackupStatus::Missing,
        },
        destination_id,
        destination_label,
        since: r.since,
    }
}

/// Whether a [`TrustGenerationRow`] is a real retained generation or the
/// stand-in for a destination that could not be asked
/// (`nest-trust-generation-status`). FFI projection of
/// [`fauna_client_backup::generations::GenerationsStatus`].
///
/// The two must never collapse. "We could not ask" rendered as "there is
/// nothing to recover" is the exact false reassurance a hostile source or a
/// dropped connection buys, and it would be read by the one user who most needs
/// the truth (`nests.md:122`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TrustGenerationStatus {
    /// A real generation the destination is retaining and will roll back to.
    Listed,
    /// The destination could not be asked. The row carries no restore address
    /// and a shell renders **no** restore affordance (`nests.md:122`).
    Unreachable,
}

/// One `nest-trust-generation-item` row on the home nest's trust facet
/// (`nests.md` § Trust facet — generation recovery), rendered in the **Now**
/// lens *after* the backup trust rows.
///
/// **This is a flattened list, and the flattening is load-bearing.** Shells do
/// not iterate destinations and then generations; they iterate rows. A
/// destination that answered contributes one row per retained generation (none
/// when it holds nothing — the healthy steady state), and a destination that
/// could **not** be asked contributes exactly one
/// [`Unreachable`](TrustGenerationStatus::Unreachable) row. That is what makes
/// invariant 1 structural rather than a rule every shell must remember: there is
/// no way to render an unreachable destination as an absence, because its
/// absence is not representable.
///
/// Like the backup rows above, deliberately no `lasts_until` / renew / History
/// twin — these are live destination reads, not folds of the signed
/// grant-event log (`nests.md:128`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TrustGenerationRow {
    pub status: TrustGenerationStatus,
    /// `BackupDestination::destination_id` — what a
    /// [`LinkedNestsAction::RestoreGeneration`](crate::LinkedNestsAction::RestoreGeneration)
    /// names. Present on both row kinds (an unreachable row still knows *which*
    /// destination went dark).
    pub destination_id: String,
    /// The destination's user-facing label via the shared
    /// [`fauna_core::format::backup_destination_label`] fallback — never
    /// re-derived per client (priority #4).
    pub destination_label: String,
    /// The custody set's reserved name (`__mail`, `__post`, `__conv/<hex>`) —
    /// half of the restore address. Empty on an `Unreachable` row.
    pub folder_name: String,
    /// Plaintext path when the superseded custody row carried one. **Genuinely
    /// optional**: `path_hash` is one-way, so a row with no `path` (S9-scrubbed, or
    /// from a non-conforming source) has none, and a shell renders the hash instead. It must never
    /// hide or skip such a row — the rows a rogue source produced are exactly
    /// the ones a user needs to see (`nests.md:123`).
    pub path: Option<String>,
    /// Hex-encoded 32-byte path hash — **the** restore address, and the fallback
    /// the `nest-trust-generation-path` leaf renders when `path` is `None`.
    /// Empty on an `Unreachable` row.
    pub path_hash: String,
    /// Hex-encoded 32-byte manifest hash of this generation. Empty on an
    /// `Unreachable` row.
    pub manifest_hash: String,
    /// `0` on an `Unreachable` row.
    pub size_bytes: i64,
    /// Unix seconds at which this generation stopped being live. `0` on an
    /// `Unreachable` row.
    pub superseded_at: i64,
    /// Unix seconds at which the generation reclaims and rollback stops being
    /// possible — `superseded_at + grace_secs` **as the destination reported
    /// it**, never a client-side copy of `T` (`nests.md:118`). `0` on an
    /// `Unreachable` row.
    pub expires_at: i64,
}

/// What a [`LinkedNestsAction::RestoreGeneration`](crate::LinkedNestsAction::RestoreGeneration)
/// did, typed so each shell renders its **own localized** copy — FFI projection
/// of [`fauna_client_backup::generations::RestoreOutcome`].
///
/// It is deliberately not folded onto `LinkedNestsSnapshot::error`:
/// [`PastRecoveryWindow`](Self::PastRecoveryWindow) is a product state, not a
/// failure, and the two need different words because only one of them is worth
/// retrying (`nests.md:124`). A shell that reported "restore failed" for a
/// generation that had simply aged past `T` would send the user hunting for a
/// bug that is not there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TrustRestoreOutcome {
    /// The generation was promoted back to live.
    Restored,
    /// The destination answered and holds no such generation — unknown, or
    /// already reclaimed past `T`. The copy says *this version is past the
    /// recovery window*, never a failure message.
    PastRecoveryWindow,
}

/// Flatten the shared per-destination read into the shell's row list, applying
/// the one rule that makes invariant 1 structural (see [`TrustGenerationRow`]).
///
/// Destination order is the client's own pinned config order, and generation
/// order within a destination is **the destination's** — newest supersede
/// first, deliberately preserved rather than re-sorted, so one nest-side
/// ordering rule serves all 7 apps (`nests.md:117`).
pub(crate) fn project_generation_rows(
    groups: Vec<fauna_client_backup::generations::DestinationGenerations>,
) -> Vec<TrustGenerationRow> {
    use fauna_client_backup::generations::GenerationsStatus;

    let mut rows = Vec::new();
    for group in groups {
        if group.status == GenerationsStatus::Unreachable {
            // Exactly one row, carrying no restore address. Note this arm is
            // reached *instead of* the loop below rather than in addition to
            // it: a failed read has no generations to enumerate, and inventing
            // rows for it would be the same lie from the other direction.
            rows.push(TrustGenerationRow {
                status: TrustGenerationStatus::Unreachable,
                destination_id: group.destination_id,
                destination_label: group.destination_label,
                folder_name: String::new(),
                path: None,
                path_hash: String::new(),
                manifest_hash: String::new(),
                size_bytes: 0,
                superseded_at: 0,
                expires_at: 0,
            });
            continue;
        }
        for g in group.generations {
            rows.push(TrustGenerationRow {
                status: TrustGenerationStatus::Listed,
                destination_id: group.destination_id.clone(),
                destination_label: group.destination_label.clone(),
                folder_name: g.folder_name,
                path: g.path,
                path_hash: g.path_hash,
                manifest_hash: g.manifest_hash,
                size_bytes: g.size_bytes,
                superseded_at: g.superseded_at,
                // Already derived from the destination's own reported
                // `grace_secs` by the shared projection — carried through
                // unchanged, never recomputed here.
                expires_at: g.expires_at,
            });
        }
    }
    rows
}

/// The backup half of the trust facet's seams — `None` on a machine whose client
/// has no backup surface wired yet, which renders the facet exactly as before
/// (no backup rows) rather than failing.
///
/// `source` is a [`BackupNestSeam`](fauna_client_backup::trust::BackupNestSeam)
/// over the **home** connection; `connector` opens one over a *destination* on
/// demand. Both are generated by
/// [`impl_backup_nest_seam!`](fauna_client_backup::impl_backup_nest_seam) in the
/// per-app glue, so no client re-derives a `fauna.backup.*` kind name.
pub struct BackupTrustSeams {
    /// The account's backup-destination state (`fauna.state.backup`) — the
    /// bound box's list the rows resolve destinations from.
    pub state: Arc<dyn fauna_client_config::BackupStateStore>,
    pub source: Arc<dyn fauna_client_backup::trust::BackupNestSeam>,
    pub connector: Arc<dyn fauna_client_backup::trust::BackupDestinationConnector>,
}

/// Whether a projected scope list marks a **bounded** (content-sealing-epochs)
/// mail grant — thin FFI-facing wrapper over
/// [`fauna_client_capabilities::grant_log::is_bounded_mail_grant`], kept here
/// because [`TrustScope`] is this crate's FFI/wasm projection of the pure
/// `GrantEventScope`, not the pure type itself (see the module doc's "Why
/// pair-local FFI rows"). The Nests-page trust facet's honest-bound copy
/// (`nests.md` § Honest bound; flip-checklist line 6) switches between the
/// standing and bounded-regime wording off this — Now-lens grant rows
/// ([`TrustGrantRow::scope`]) and History-lens entries ([`TrustHistoryRow::scope`])
/// alike. A hint, not an enforcer (see `is_bounded_mail_grant`'s own doc) —
/// never re-derive the (class, kind, tier) check per client (priority #2).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn trust_scope_is_bounded_mail_grant(scope: Vec<TrustScope>) -> bool {
    let converted: Vec<GrantEventScope> = scope
        .into_iter()
        .map(|s| GrantEventScope {
            class: s.class,
            kind: s.kind,
            tier: s.tier,
        })
        .collect();
    fauna_client_capabilities::grant_log::is_bounded_mail_grant(&converted)
}

// ── shell-facing labels: shared enum → i18n key mapping ─────────────
//
// Each maps a shared trust type to a `LocalizedText` — a key plus (optional)
// substitution args, never resolved text (`fauna_core::localized`'s module
// doc) — which every app resolves through its own i18n runtime. Lifts the
// identical match arms tui and linux each hard-coded, mirroring
// `fauna_client_mail_settings::lists::member_status_label`.

/// Localized label for a [`TrustScope`] tuple (`nest-trust-grant-scope` /
/// `nest-trust-history-item`'s scope segment). `content.read{mail|calendar|
/// post}` is the v1 vocabulary; anything else falls back to a `LocalizedText`
/// carrying the raw kind/class as its own key, so an as-yet-unlabeled scope
/// still renders honestly (via `resolve`'s missing-key fallback) rather than
/// blank.
///
/// A **per-labeler** mail grant — a `wasm` mail-labeler subscription's twin,
/// whose tuples carry the labeler's factor folded into `kind`
/// (`GrantEventScope::factor`; `nests.md` § Trust facet — grants → *the
/// per-labeler grant*) — names the labeler it is confined to, so it never
/// reads as a second anonymous "Mail" row beside the composed one.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn scope_label(s: &TrustScope) -> LocalizedText {
    let folded = GrantEventScope {
        class: s.class.clone(),
        kind: s.kind.clone(),
        tier: s.tier.clone(),
    };
    let labeler = folded
        .factor()
        .and_then(fauna_core::scoring::labeler_factor_id)
        .map(|id| fauna_core::format::short_id(&id.to_hex()));
    if s.class == "content.read" {
        match folded.base_kind() {
            Some("mail") if labeler.is_some() => {
                return LocalizedText::key_arg(
                    "nests.scope_mail_labeler",
                    "labeler",
                    labeler.unwrap_or_default(),
                );
            }
            Some("mail") => return LocalizedText::key("nests.scope_mail"),
            Some("calendar") => return LocalizedText::key("nests.scope_calendar"),
            // The keyless grant the spam page's contribute switch mints
            // (`mail-spam.md` § Encrypted-mode interaction).
            Some("spam-model") => return LocalizedText::key("nests.scope_spam_model"),
            // A tier-scoped post grant names WHICH tier's posts — two tiers
            // must stay distinguishable in the audit view.
            Some("post") => {
                return match s.tier.as_deref() {
                    Some(tier) => LocalizedText::key_arg("nests.scope_posts_tier", "tier", tier),
                    None => LocalizedText::key("nests.scope_posts"),
                };
            }
            _ => {}
        }
    }
    // content.label-write (bundled into the Mail mint option) — never the raw
    // scope-class string in a grant row.
    if s.class == "content.label-write" {
        return match labeler {
            Some(labeler) => {
                LocalizedText::key_arg("nests.scope_labeler_labels", "labeler", labeler)
            }
            None => LocalizedText::key("nests.scope_spam_labels"),
        };
    }
    LocalizedText::key(s.kind.clone().unwrap_or_else(|| s.class.clone()))
}

/// [`scope_label`] for a third-party principal's grant, whose folder read
/// tuple names the folder it covers — `folder` resolved above the fold
/// (`fauna_client_capabilities::folder_principal_set_names` →
/// `view_model::principal_grant_folder`), since the signed log never says
/// which (`webdav-server.md` § Key model → *A principal's read* rule (1)).
/// Any other tuple, or no folder, is [`scope_label`]'s own.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn scope_label_in_folder(s: &TrustScope, folder: Option<TrustFolder>) -> LocalizedText {
    let is_folder_read = s.class == "content.read"
        && GrantEventScope {
            class: s.class.clone(),
            kind: s.kind.clone(),
            tier: s.tier.clone(),
        }
        .base_kind()
            == Some("folder");
    match folder {
        Some(folder) if is_folder_read => folder_label(folder),
        _ => scope_label(s),
    }
}

fn folder_label(folder: TrustFolder) -> LocalizedText {
    match folder {
        TrustFolder::Named { name } => LocalizedText::key_arg("nests.scope_folder", "folder", name),
        TrustFolder::Deleted => LocalizedText::key("nests.scope_folder_deleted"),
    }
}

/// The labels a grant row's or History line's scope renders as, one per tuple
/// through [`scope_label_in_folder`] — what every shell joins into its scope
/// line. A `Revoke` carries no scope, so one whose grant names a folder
/// ([`TrustHistoryRow::folder`]) renders that folder alone.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn grant_scope_labels(
    scope: Vec<TrustScope>,
    folder: Option<TrustFolder>,
) -> Vec<LocalizedText> {
    if scope.is_empty() {
        return folder.map(folder_label).into_iter().collect();
    }
    scope
        .iter()
        .map(|s| scope_label_in_folder(s, folder.clone()))
        .collect()
}

/// Localized label for a grant's [`TrustLiveness`] (`nest-trust-grant-status`).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn status_label(l: TrustLiveness) -> LocalizedText {
    LocalizedText::key(match l {
        TrustLiveness::Active => "nests.status_active",
        TrustLiveness::ExpiringSoon => "nests.status_expiring",
        TrustLiveness::Expired => "nests.status_expired",
        TrustLiveness::AutoRenewing => "nests.status_auto_renewing",
    })
}

/// Localized label for a backup row's [`TrustBackupStatus`]
/// (`nest-trust-backup-status`). `Unreachable` ("we could not ask the
/// destination") stays distinct from `Missing` ("it answered and holds no such
/// trust") — collapsing them would let a flaky network read as a revoked
/// backup.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn backup_status_label(status: TrustBackupStatus) -> LocalizedText {
    LocalizedText::key(match status {
        TrustBackupStatus::Active => "nests.backup_status_active",
        TrustBackupStatus::Unreachable => "nests.backup_status_unreachable",
        TrustBackupStatus::Missing => "nests.backup_status_missing",
    })
}

/// One History-lens row's self-describing line ("Trusted to read ‹scope› ·
/// ‹when›" etc., `nest-trust-history-item`). `scope`/`when` are the caller's
/// own already-resolved display strings ([`scope_label`] joined, and the
/// shared local-date/time render) — this door owns only which `history_*`
/// template the event kind picks. Shared decision (tui↔linux twin harvest,
/// previously hand-rolled identically on both apps).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn history_line_text(kind: TrustEventKind, scope: &str, when: &str) -> LocalizedText {
    let args = [("scope", scope), ("when", when)];
    match kind {
        TrustEventKind::Mint => LocalizedText::key_args("nests.history_minted", args),
        TrustEventKind::Renew => LocalizedText::key_args("nests.history_renewed", args),
        TrustEventKind::Revoke => LocalizedText::key_args("nests.history_revoked", args),
    }
}

/// Localized label for a mint-picker option's use case
/// (`nest-trust-mint-scope-select`). The paywalled option carries its tier via
/// the `{tier}` named placeholder.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn mint_option_label(o: &TrustMintOption) -> LocalizedText {
    match o.use_case {
        TrustMintUseCase::Mail => LocalizedText::key("nests.mint_option_mail"),
        TrustMintUseCase::Calendar => LocalizedText::key("nests.mint_option_calendar"),
        TrustMintUseCase::PaywalledPosts => LocalizedText::key_arg(
            "nests.mint_option_paywalled",
            "tier",
            o.tier.clone().unwrap_or_default(),
        ),
    }
}

#[cfg(test)]
mod label_tests {
    use super::*;

    fn scope(class: &str, kind: Option<&str>, tier: Option<&str>) -> TrustScope {
        TrustScope {
            class: class.to_string(),
            kind: kind.map(String::from),
            tier: tier.map(String::from),
        }
    }

    #[test]
    fn scope_label_maps_every_content_read_kind() {
        assert_eq!(
            scope_label(&scope("content.read", Some("mail"), None)).key,
            "nests.scope_mail"
        );
        assert_eq!(
            scope_label(&scope("content.read", Some("calendar"), None)).key,
            "nests.scope_calendar"
        );
        assert_eq!(
            scope_label(&scope("content.read", Some("post"), None)).key,
            "nests.scope_posts"
        );
    }

    #[test]
    fn scope_label_names_the_tier_on_a_tier_scoped_post_grant() {
        let lt = scope_label(&scope("content.read", Some("post"), Some("gold")));
        assert_eq!(lt.key, "nests.scope_posts_tier");
        assert_eq!(lt.args.get("tier").map(String::as_str), Some("gold"));
    }

    /// The keyless spam-model grant the "Contribute to deployment spam baseline"
    /// switch mints is listed in the owner's grant log like any other — under a
    /// label saying what it lets the nest read, never the raw `spam-model` kind.
    #[test]
    fn scope_label_names_the_spam_model_grant() {
        assert_eq!(
            scope_label(&scope("content.read", Some("spam-model"), None)).key,
            "nests.scope_spam_model"
        );
    }

    /// A per-labeler mail grant's two tuples each name the labeler they are
    /// confined to (its short id), so the Now/History lenses render "run
    /// labeler ‹id› over my mail" rather than a second anonymous mail row.
    #[test]
    fn scope_label_names_the_labeler_on_a_per_labeler_mail_grant() {
        let labeler = fauna_core::identity::ActorId([0xABu8; 32]);
        let scope =
            fauna_client_capabilities::grant_log::bounded_mail_labeler_event_scope(&labeler);
        let short = fauna_core::format::short_id(&labeler.to_hex());
        let mail = scope_label(&project_scope(scope[0].clone()));
        assert_eq!(mail.key, "nests.scope_mail_labeler");
        assert_eq!(
            mail.args.get("labeler").map(String::as_str),
            Some(short.as_str())
        );
        let labels = scope_label(&project_scope(scope[1].clone()));
        assert_eq!(labels.key, "nests.scope_labeler_labels");
        assert_eq!(
            labels.args.get("labeler").map(String::as_str),
            Some(short.as_str())
        );
        // …and the same scope still reads as bounded for the honest-bound copy.
        assert!(trust_scope_is_bounded_mail_grant(
            scope.into_iter().map(project_scope).collect()
        ));
    }

    /// A principal's folder read grant names its folder — or that the folder
    /// is gone — instead of the bare `folder` kind; any other scope keeps its
    /// own label whatever folder is passed.
    #[test]
    fn scope_label_in_folder_names_a_principals_folder() {
        let folder_read = scope("content.read", Some("folder"), None);
        let named = scope_label_in_folder(
            &folder_read,
            Some(TrustFolder::Named {
                name: "photos".into(),
            }),
        );
        assert_eq!(named.key, "nests.scope_folder");
        assert_eq!(named.args.get("folder").map(String::as_str), Some("photos"));
        assert_eq!(
            scope_label_in_folder(&folder_read, Some(TrustFolder::Deleted)).key,
            "nests.scope_folder_deleted"
        );
        assert_eq!(
            scope_label_in_folder(&folder_read, None),
            scope_label(&folder_read)
        );
        assert_eq!(
            scope_label_in_folder(
                &scope("content.read", Some("mail"), None),
                Some(TrustFolder::Deleted)
            )
            .key,
            "nests.scope_mail"
        );
        assert_eq!(
            TrustFolder::from(PrincipalFolder::Named("photos".into())),
            TrustFolder::Named {
                name: "photos".into()
            }
        );
    }

    #[test]
    fn grant_scope_labels_name_the_folder_even_on_a_scopeless_revoke() {
        let premium = Some(TrustFolder::Named {
            name: "premium".into(),
        });
        let keys = |labels: Vec<LocalizedText>| {
            labels
                .into_iter()
                .map(|l| (l.key, l.args.get("folder").cloned()))
                .collect::<Vec<_>>()
        };
        let named = (
            "nests.scope_folder".to_string(),
            Some("premium".to_string()),
        );
        assert_eq!(
            keys(grant_scope_labels(
                vec![scope("content.read", Some("folder"), None)],
                premium.clone()
            )),
            std::slice::from_ref(&named)
        );
        assert_eq!(keys(grant_scope_labels(Vec::new(), premium)), [named]);
        assert!(grant_scope_labels(Vec::new(), None).is_empty());
        assert_eq!(
            keys(grant_scope_labels(
                vec![scope("content.read", Some("mail"), None)],
                None
            )),
            [("nests.scope_mail".to_string(), None)]
        );
    }

    #[test]
    fn scope_label_maps_label_write_and_falls_back_honestly() {
        assert_eq!(
            scope_label(&scope("content.label-write", None, None)).key,
            "nests.scope_spam_labels"
        );
        // An unlabeled scope falls back to its own kind/class as a literal
        // key — `resolve`'s missing-key fallback renders it verbatim rather
        // than going blank.
        assert_eq!(
            scope_label(&scope("content.read", Some("unknown"), None)).key,
            "unknown"
        );
        assert_eq!(
            scope_label(&scope("some.other.class", None, None)).key,
            "some.other.class"
        );
    }

    #[test]
    fn status_label_maps_every_liveness() {
        assert_eq!(
            status_label(TrustLiveness::Active).key,
            "nests.status_active"
        );
        assert_eq!(
            status_label(TrustLiveness::ExpiringSoon).key,
            "nests.status_expiring"
        );
        assert_eq!(
            status_label(TrustLiveness::Expired).key,
            "nests.status_expired"
        );
        assert_eq!(
            status_label(TrustLiveness::AutoRenewing).key,
            "nests.status_auto_renewing"
        );
    }

    #[test]
    fn backup_status_label_maps_every_status() {
        assert_eq!(
            backup_status_label(TrustBackupStatus::Active).key,
            "nests.backup_status_active"
        );
        assert_eq!(
            backup_status_label(TrustBackupStatus::Unreachable).key,
            "nests.backup_status_unreachable"
        );
        assert_eq!(
            backup_status_label(TrustBackupStatus::Missing).key,
            "nests.backup_status_missing"
        );
    }

    #[test]
    fn history_line_text_maps_every_kind_and_carries_the_scope_and_when_args() {
        for (kind, key) in [
            (TrustEventKind::Mint, "nests.history_minted"),
            (TrustEventKind::Renew, "nests.history_renewed"),
            (TrustEventKind::Revoke, "nests.history_revoked"),
        ] {
            let lt = history_line_text(kind, "mail", "2026-08-20");
            assert_eq!(lt.key, key, "{kind:?}");
            assert_eq!(lt.args.get("scope").map(String::as_str), Some("mail"));
            assert_eq!(lt.args.get("when").map(String::as_str), Some("2026-08-20"));
        }
    }

    #[test]
    fn mint_option_label_maps_every_use_case() {
        assert_eq!(
            mint_option_label(&TrustMintOption {
                use_case: TrustMintUseCase::Mail,
                tier: None,
                scope: Vec::new(),
                holder_candidates: Vec::new(),
            })
            .key,
            "nests.mint_option_mail"
        );
        assert_eq!(
            mint_option_label(&TrustMintOption {
                use_case: TrustMintUseCase::Calendar,
                tier: None,
                scope: Vec::new(),
                holder_candidates: Vec::new(),
            })
            .key,
            "nests.mint_option_calendar"
        );
        let paywalled = mint_option_label(&TrustMintOption {
            use_case: TrustMintUseCase::PaywalledPosts,
            tier: Some("gold".to_string()),
            scope: Vec::new(),
            holder_candidates: Vec::new(),
        });
        assert_eq!(paywalled.key, "nests.mint_option_paywalled");
        assert_eq!(paywalled.args.get("tier").map(String::as_str), Some("gold"));
    }
}

// ── projections: pure `view_model` → FFI rows ───────────────────────

fn project_scope(s: GrantEventScope) -> TrustScope {
    TrustScope {
        class: s.class,
        kind: s.kind,
        tier: s.tier,
    }
}

fn project_liveness(l: GrantLiveness) -> TrustLiveness {
    match l {
        GrantLiveness::Active => TrustLiveness::Active,
        GrantLiveness::ExpiringSoon => TrustLiveness::ExpiringSoon,
        GrantLiveness::Expired => TrustLiveness::Expired,
        GrantLiveness::AutoRenewing => TrustLiveness::AutoRenewing,
    }
}

fn project_event_kind(k: GrantEventKind) -> TrustEventKind {
    match k {
        GrantEventKind::Mint => TrustEventKind::Mint,
        GrantEventKind::Renew => TrustEventKind::Renew,
        GrantEventKind::Revoke => TrustEventKind::Revoke,
    }
}

/// Project one live grant, joining it to the post-succession review marks the
/// succession ledger carries (`fauna_core::data::GrantUnattestedMark`).
///
/// The join is by `grant_id`, and it is exact after a re-mint rather than
/// heuristic: the driver moves each mark onto the *replacement* id it derives,
/// so a re-minted grant and its mark name the same handle. A grant with no
/// mark is the ordinary case and projects `unattested: false`.
pub(crate) fn project_grant(
    g: GrantView,
    marks: &[fauna_core::data::GrantUnattestedMark],
    folders: Option<&FolderSetNames>,
) -> TrustGrantRow {
    // The shared reading, not a local one: a grant carried across two
    // successions holds one mark per raising event, and "is this row still
    // asking?" is *any open mark*, never *any mark* (a decided one stays at
    // rest — `UnattestedVerdict`).
    let unattested = fauna_core::data::GrantUnattestedMark::any_open(marks, &g.grant_id);
    let folder = folder_of(&g.grant_id, &g.scope, folders);
    TrustGrantRow {
        grant_id: g.grant_id,
        holder: g.holder,
        scope: g.scope.into_iter().map(project_scope).collect(),
        lasts_until: g.lasts_until as i64,
        liveness: project_liveness(g.liveness),
        unattested,
        folder,
    }
}

/// The owner's folder grant id → set-name map ([`TrustFolderNames::resolve`]).
pub(crate) type FolderSetNames = std::collections::BTreeMap<[u8; 16], String>;

/// A grant's folder through the shared [`view_model::principal_grant_folder`],
/// or `None` when the set names could not be resolved — never a `Deleted`
/// guessed from a map that was never read.
fn folder_of(
    grant_id: &[u8],
    scope: &[GrantEventScope],
    folders: Option<&FolderSetNames>,
) -> Option<TrustFolder> {
    view_model::principal_grant_folder(grant_id, scope, folders?).map(TrustFolder::from)
}

pub(crate) fn project_mint_option(
    o: fauna_client_capabilities::view_model::MintOptionModel,
) -> TrustMintOption {
    use fauna_client_capabilities::view_model::MintUseCase;
    TrustMintOption {
        use_case: match o.use_case {
            MintUseCase::Mail => TrustMintUseCase::Mail,
            MintUseCase::Calendar => TrustMintUseCase::Calendar,
            MintUseCase::PaywalledPosts => TrustMintUseCase::PaywalledPosts,
        },
        tier: o.tier,
        scope: o.scope.into_iter().map(project_scope).collect(),
        holder_candidates: o.holder_candidates,
    }
}

pub(crate) fn project_history(
    h: HistoryEntry,
    folders: Option<&FolderSetNames>,
) -> TrustHistoryRow {
    let folder = folder_of(&h.grant_id, &h.scope, folders);
    TrustHistoryRow {
        folder,
        grant_id: h.grant_id,
        holder: h.holder,
        kind: project_event_kind(h.kind),
        scope: h.scope.into_iter().map(project_scope).collect(),
        window_start: h.window_start as i64,
        window_end: h.window_end as i64,
        at: h.at as i64,
    }
}

/// Map a UI-supplied [`TrustScope`] to the crate's mint-side
/// [`fauna_mls::wrapped_blob::ScopeTuple`] (the type `mint_grant` derives keys
/// for). The inverse of [`project_scope`], used when a `Mint` action carries
/// the user's chosen scope down to the client-side mint.
pub(crate) fn scope_tuple_from(s: &TrustScope) -> fauna_mls::wrapped_blob::ScopeTuple {
    fauna_mls::wrapped_blob::ScopeTuple {
        class: s.class.clone(),
        kind: s.kind.clone(),
        tier: s.tier.clone(),
        set: None,
        factor: None,
    }
}

/// Map a UI-supplied [`TrustScope`] to the log's
/// [`fauna_core::grant_event::GrantEventScope`] (recorded verbatim in the
/// signed `Mint` event).
pub(crate) fn grant_event_scope_from(s: &TrustScope) -> GrantEventScope {
    GrantEventScope {
        class: s.class.clone(),
        kind: s.kind.clone(),
        tier: s.tier.clone(),
    }
}

// ── seams ───────────────────────────────────────────────────────────

// `HolderInfo` + `discover_holders` moved to `fauna-client-bridges`
// (2026-07-19), the home of `MailAdminClient` — so the mail-settings
// rotation-heal driver shares the same discovery implementation without
// depending on this crate (priority #2). `lib.rs` re-exports
// `fauna_client_bridges::HolderInfo` to preserve the `fauna_client_pair::HolderInfo`
// path.

/// User's Ed25519 signing-key seam for the grant-event log — the **shared**
/// `fauna_client_capabilities::grant_log::GrantEventSigner` (the labeler
/// catalog's mint signs through the same seam), re-exported under the path
/// every glue and test already names. The impl holds the raw `SigningKey` and
/// signs a fully-populated (placeholder-`sig`)
/// [`GrantEvent`](fauna_core::grant_event::GrantEvent); the raw key never
/// crosses the machine's FFI boundary (`key-material-hierarchy.md` #7).
pub use fauna_client_capabilities::grant_log::GrantEventSigner;

/// Platform-supplied clock + randomness so the crate stays clockless /
/// wasm-clean (the pure folds already take `now` as a parameter; the machine
/// gets it here). `now_epoch_secs` stamps grant windows + event `at`;
/// `new_grant_id` mints the 16-byte opaque grant handle.
pub trait TrustPlatform: MaybeSendSync {
    /// Seconds since the Unix epoch — the grant window + event-log clock unit
    /// (`bins/fauna-nest/src/db/mod.rs::now_epoch_secs`).
    fn now_epoch_secs(&self) -> u64;
    /// A fresh 16-byte grant id (CSPRNG). Distinct per mint; collision is a
    /// (owner, grant_id) storage-key clash, so it must be random, not a counter.
    fn new_grant_id(&self) -> [u8; 16];
}

/// Where the user's per-nest blessing verdicts live (`nest-trust-blessed-toggle`;
/// `nests.md` § Expiry / renewal → *Duration and blessing*): the account
/// plane's `fauna.state.blessed-nests` kind, one row per nest
/// (`config-dissolution.md` — the kinds table's row; the kind is born
/// plane-only). The one
/// impl every runtime-hosting app wires is
/// `fauna_account_seams::blessed_nests::PlaneBlessedNests`, over the seat's
/// account-store handle; this crate never learns about the account runtime.
///
/// `nest_id` is the identity the connection PROVED (`bound_nest_id`), never a
/// nest's own claim. An `Err` is the store's refusal or its absence (no
/// account runtime yet) — the machine renders an unreadable verdict as
/// un-blessed and surfaces a failed write.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait BlessedNestsStore: MaybeSendSync {
    /// Whether the user has blessed `nest_id` (an absent verdict is not).
    async fn is_blessed(&self, nest_id: &[u8; 32]) -> Result<bool, String>;
    /// Record the user's verdict for `nest_id` at `now` (seconds since the
    /// Unix epoch). Returns whether anything changed — re-asserting the
    /// standing verdict writes nothing.
    async fn set_blessed(
        &self,
        nest_id: &[u8; 32],
        blessed: bool,
        now: u64,
    ) -> Result<bool, String>;
}

/// The [`BlessedNestsStore`] of a seat that hosts no account runtime (a build
/// without one, a harness driving the nest alone): there is no plane to hold
/// a verdict, so every read is refused (rendered un-blessed) and every write
/// refused — never kept anywhere else, the kind being plane-only.
pub struct NoAccountRuntime;

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl BlessedNestsStore for NoAccountRuntime {
    async fn is_blessed(&self, _nest_id: &[u8; 32]) -> Result<bool, String> {
        Err(NO_ACCOUNT_RUNTIME.into())
    }

    async fn set_blessed(
        &self,
        _nest_id: &[u8; 32],
        _blessed: bool,
        _now: u64,
    ) -> Result<bool, String> {
        Err(NO_ACCOUNT_RUNTIME.into())
    }
}

const NO_ACCOUNT_RUNTIME: &str = "this seat hosts no account runtime";

/// The bundle of trust-facet seams, wired once at machine construction. Grouped
/// so `LinkedNestsMachine::new` (pairing-only) stays a two-arg constructor and
/// the trust-enabled path takes one extra `Option<TrustSeams>` — a machine
/// built without it renders pairings only (existing clients, unchanged), and a
/// trust action on it errors `InvalidState` rather than panicking.
pub struct TrustSeams {
    /// The owner's identity pubkey (`mint_grant`'s `owner_actor_id`).
    pub actor_id: [u8; 32],
    /// The grant-event log and its marks — `fauna.state.succession-ledger`,
    /// through the host's account-store handle. Before the store is up it
    /// refuses, and a trust action fails rather than recording nowhere.
    pub ledger: Arc<dyn SuccessionLedgerStore>,
    /// The owner's period-key custody (`fauna.state.subscriptions`) — what a
    /// post-tier grant wraps and which tiers the mint picker offers. A store
    /// that cannot be read yet offers no post tier and mints none.
    pub period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
    /// The account's mail custody (`fauna.state.mail`) — the MSEK a
    /// mail/calendar grant's payload, and a bounded grant's epoch wraps,
    /// derive from.
    pub mail: Arc<dyn fauna_client_config::MailStore>,
    pub signer: Arc<dyn GrantEventSigner>,
    pub platform: Arc<dyn TrustPlatform>,
    /// The per-nest blessing verdicts ([`BlessedNestsStore`]) — the toggle,
    /// the one-tap default set and the auto-renew sweep read and write here.
    pub blessings: Arc<dyn BlessedNestsStore>,
    /// The backup half (`nest-trust-backup-item` rows + their revokes). `None`
    /// ⇒ the facet renders content-processing grants only, exactly as before
    /// the backup rows landed, and a backup action errors `InvalidState`.
    pub backup: Option<BackupTrustSeams>,
    /// What names a folder grant's folder on the facet
    /// ([`TrustGrantRow::folder`]). `None` ⇒ no row names one, and a folder
    /// grant renders as a bare folder read.
    pub folder_names: Option<TrustFolderNames>,
}

/// The seam that names the folder a web-serve paywall grant covers: the
/// owner's set names, walked by
/// [`fauna_client_capabilities::folder_paywall_set_names`] under the owner
/// secret that derives each generation's grant id (`webdav-server.md` § Key
/// model → *A principal's read* rule (1) → *The generation*). The secret stays
/// inside this struct; no row carries it.
pub struct TrustFolderNames {
    owner: fauna_core::identity::ActorKeypair,
    sets: Arc<dyn fauna_client_capabilities::OwnedSetNames>,
}

impl TrustFolderNames {
    /// The seam for the owner `keypair` over `sets`.
    pub fn new(
        keypair: &fauna_core::identity::ActorKeypair,
        sets: Arc<dyn fauna_client_capabilities::OwnedSetNames>,
    ) -> Self {
        Self {
            owner: fauna_core::identity::ActorKeypair::from_secret(*keypair.secret_bytes()),
            sets,
        }
    }

    /// Every paywall grant id the log `events` hold, at every generation,
    /// mapped to its set's name. `None` when the owner's set names cannot be
    /// read now.
    pub(crate) async fn resolve(
        &self,
        events: &[fauna_core::grant_event::GrantEvent],
    ) -> Option<FolderSetNames> {
        let set_names = self.sets.owned_set_names().await?;
        Some(fauna_client_capabilities::folder_paywall_set_names(
            events,
            self.owner.secret_bytes(),
            &set_names,
        ))
    }
}

#[cfg(test)]
mod project_grant_tests {
    use super::*;
    use fauna_client_capabilities::view_model::{GrantLiveness, GrantView};
    use fauna_core::data::GrantUnattestedMark;
    use fauna_core::identity::ActorId;

    fn view(grant_id: &[u8]) -> GrantView {
        GrantView {
            grant_id: grant_id.to_vec(),
            holder: vec![9; 32],
            scope: Vec::new(),
            lasts_until: 4_102_444_800,
            liveness: GrantLiveness::Active,
        }
    }

    fn mark(grant_id: &[u8]) -> GrantUnattestedMark {
        GrantUnattestedMark {
            grant_id: grant_id.to_vec(),
            predecessor: ActorId([7; 32]),
            verdict: fauna_core::data::UnattestedVerdict::Open,
        }
    }

    fn decided(grant_id: &[u8]) -> GrantUnattestedMark {
        GrantUnattestedMark {
            verdict: fauna_core::data::UnattestedVerdict::Kept,
            ..mark(grant_id)
        }
    }

    /// The join that carries a succession-ledger review mark onto the live grant row
    /// the shell renders (`succession-aftermath.md` § Adjudicating what the
    /// aftermath carries across).
    ///
    /// This is the seam every one of the seven apps depends on and none of them
    /// can see: a shell fixture that sets `unattested` by hand still passes with
    /// the join severed, so without this test a re-mint could silently restore
    /// read capability with no mark anywhere. Asserted in BOTH directions,
    /// because a join stuck on `true` is as wrong as one stuck on `false` — it
    /// would mark every ordinary grant and train the user past the real one.
    #[test]
    fn a_marked_grant_projects_unattested_and_an_unmarked_one_does_not() {
        let marks = vec![mark(&[1; 16])];

        assert!(
            project_grant(view(&[1; 16]), &marks, None).unattested,
            "a grant the aftermath carried across must reach the shell marked"
        );
        assert!(
            !project_grant(view(&[2; 16]), &marks, None).unattested,
            "a grant with no mark is the ordinary case and must render nothing"
        );
        assert!(
            !project_grant(view(&[1; 16]), &[], None).unattested,
            "an empty mark list marks nothing — the state of every account that \
             never succeeded"
        );

        // Since 2026-08-06 an answered mark STAYS at rest carrying its verdict,
        // so "is this row still asking?" is *any open mark*, never *any mark* —
        // reading presence alone would re-ask every question the owner has
        // already closed, on every device, forever.
        assert!(
            !project_grant(view(&[1; 16]), &[decided(&[1; 16])], None).unattested,
            "a Kept mark is at rest, not open — the row must stop asking"
        );
        assert!(
            project_grant(view(&[1; 16]), &[decided(&[1; 16]), mark(&[1; 16])], None).unattested,
            "a LATER succession's open mark still asks, even beside a closed one"
        );
    }
}
