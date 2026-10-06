//! Shared orchestration for the user-settings **Linked nests** page — where a
//! user links one of their own nests to sync their account's content (per-user
//! multi-homing), lists their linked nests, and unlinks them.
//!
//! Authority for behavior + the shared-Rust/client split:
//! `docs/goal/behavior/linked-nests.md` § Where logic lives. Wire shape,
//! capabilities, and the bearer kinds: `docs/goal/architecture/nest/
//! private-mode.md` § Pairing Flow (design ratified, tracked internally).
//! Authority for UX/IDs: `tests/e2e-unified/ui.yaml` `linked-nests`.
//!
//! Per priority #2 the snapshot projection + the link/list/unlink sequencing
//! live here, not in any per-app shell: the UI renders [`LinkedNestsSnapshot`]
//! and dispatches [`LinkedNestsAction`]; the per-app glue implements one
//! WS-RPC seam ([`LinkedNestsNest`]) over the **bearer** WS-RPC handle (these are
//! `User`-gated owner-scoped kinds, unlike `fauna-client-dns`'s Admin kinds).
//! Mirrors `fauna-client-dns`'s `DnsManagementMachine` (snapshot, action, seam,
//! machine — TDD'd against a fake `Nest`), but simpler: one nest seam, no
//! client-side provider/credential store.
//!
//! Surface:
//!   * [`LinkedNestsAction::Refresh`] (`fauna.pair.list`) — the owner's active
//!     pairings, projected into [`LinkedNestRow`].
//!   * [`LinkedNestsAction::Link`] (`fauna.pair.add`) — authorize one of the
//!     user's nests to sync their account; defaults to the full self-sync
//!     capability set ([`fauna_protocol::pair::default_self_sync`]) when the UI
//!     passes none. Re-lists on success.
//!   * [`LinkedNestsAction::Unlink`] (`fauna.pair.revoke`) — revoke a pairing;
//!     re-lists on success.
//!
//! The nest identity the user enters/scans is a 32-byte Ed25519 public key in
//! hex; the machine parses it here (priority #2) so every app validates it
//! identically. An out-of-band short-code form is a future refinement (the
//! design names it but does not fix a format yet).

// `LinkedNestsNest`/`PostLinkHook` and the store seams are bounded by
// `MaybeSendSync` (`Send + Sync` natively, empty on wasm32), so an
// `Arc<dyn ...>`-holding seam struct is correctly `!Send`/`!Sync` on wasm32
// but trips `arc_with_non_send_sync` there. wasm32-scoped so native, where
// the bound resolves to `Send + Sync`, keeps the lint's protection.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_client_pair");

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_protocol::pair::{
    ForwardQueue, PairAddReply, PairAddRequest, PairForwardDiscardReply, PairForwardDiscardRequest,
    PairForwardRetryReply, PairForwardRetryRequest, PairListReply, PairListRequest,
    PairRevokeReply, PairRevokeRequest, PairingRow, default_self_sync,
};
use fauna_protocol::{ByteBuf, MaybeSendSync, RpcErrorClass, RpcRequester};
// The snapshot/row/enums + the action enum cross the JS boundary as `JsValue`
// for the wasm consumer (`libs/fauna-wasm`'s `WasmLinkedNestsMachine`): the
// snapshot via `serde_wasm_bindgen` on `snapshot()`, the action on `dispatch`.
use serde::{Deserialize, Serialize};

mod error;
pub use error::{PairDispatchError, PairNestError, StoreError, TrustSignerError};

pub mod trust;
pub use trust::{
    BackupStateStore, BackupTrustSeams, BlessedNestsStore, GrantEventSigner, NoAccountRuntime,
    SuccessionLedgerStore, TrustBackupKind, TrustBackupRow, TrustBackupStatus, TrustEventKind,
    TrustFolder, TrustFolderNames, TrustGenerationRow, TrustGenerationStatus, TrustGrantDuration,
    TrustGrantRow, TrustHistoryRow, TrustLens, TrustLiveness, TrustMintOption, TrustMintUseCase,
    TrustPlatform, TrustRestoreOutcome, TrustScope, TrustSeams, auto_renew_check_secs,
    backup_status_label, duration_label, grant_scope_labels, history_line_text,
    mint_duration_options, mint_option_label, scope_label, scope_label_in_folder, status_label,
    trust_scope_is_bounded_mail_grant,
};
// `HolderInfo` + `discover_holders` now live in `fauna-client-bridges` (the home
// of `MailAdminClient`); re-exported here so `fauna_client_pair::HolderInfo` keeps
// resolving for existing consumers (2026-07-19 lift, priority #2).
pub use fauna_client_bridges::HolderInfo;

use std::collections::BTreeSet;

// Trust facet (Nests page) shared logic: the roster discovery + the pure
// mint / grant-log / view-model folds. All wasm-clean (see Cargo.toml).
// `MailAdminClient` (not `BridgesClient`) carries `list_service_users` +
// `fetch_bridge_pubkey` — the service-user roster the DKIM/TLS seal already
// rides (`nests.md:106`). ⚠ `list_service_users` is Admin-gated on the nest
// (`bridge_method_allowlist.rs:94`), so v1 holder discovery is admin-scoped; a
// User-callable content-processor list is a nest follow-on (see NEXT).
use fauna_client_bridges::{MailAdminClient, discover_holders};
use fauna_client_capabilities::rpc::CapabilitiesClient;
use fauna_client_capabilities::{DEFAULT_GRANT_WINDOW_SECS, grant_log, mint_grant, view_model};
use fauna_client_core::succession_delivery::OwedReason;
use fauna_core::identity::ActorId;
use fauna_core::succession_ledger::SuccessionLedger;
use fauna_mls::wrapped_blob::{GrantWindow, ScopeTuple};

/// A grant renewal on its way to the nest's `renew_grant`:
/// (grant id, window start, window end, appended wrap keys).
type GrantRenewal = ([u8; 16], u64, u64, Vec<Vec<u8>>);

// ── snapshot + action ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum LinkedNestStatus {
    Idle,
    /// Initial / refresh list fetch (`Refresh`) — the page may show a spinner.
    Loading,
    /// A link/unlink mutation in flight over the rendered list.
    Working,
}

/// One linked nest, projected from the wire [`PairingRow`] for the UI. The
/// nest's Ed25519 identity is rendered as a hex string (the page abbreviates
/// it); `capabilities` are the canonical `mls_pull` / `namespace_sync` /
/// `post_forward` set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LinkedNestRow {
    /// Hex-encoded Ed25519 public key of the linked nest (`private_nest_id`), or
    /// of the connected nest itself when [`is_home`](Self::is_home).
    pub nest_id: String,
    pub capabilities: Vec<String>,
    /// What the capabilities line (`nests-item-capabilities`) shows, one entry
    /// per [`capabilities`](Self::capabilities) entry in the same order
    /// ([`capability_label`]). Every shell resolves and joins **these**, never
    /// the wire names, so the map lives once.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = []))]
    pub capability_labels: Vec<fauna_core::localized::LocalizedText>,
    /// Optional expiry timestamp (ms epoch); after it, sync is rejected.
    pub expires_at: Option<i64>,
    pub created_at: i64,
    /// User-supplied display name for the linked nest (the list's primary label).
    pub label: Option<String>,
    /// The linked nest's address, when known (carried from the link step).
    pub nest_url: Option<String>,
    /// True for the connected/home nest row (no Unlink; it is the user's own
    /// nest, not a pairing). Pairing rows are `false`. (`nests.md` § Layout — the
    /// list is "one per pairing plus the home nest".)
    pub is_home: bool,
    /// Trust facet — Now lens: the nest's current content-processing grants
    /// (`view_model::trust_facet_for_holders` → projected). Empty ⇒ the
    /// `nest-trust-empty` state. v1 populates this only for the home row; linked
    /// nests stay empty until per-linked-nest holder enumeration lands.
    pub trust_grants: Vec<TrustGrantRow>,
    /// Trust facet — History lens: the nest's grant-event timeline
    /// (`view_model::history_for_holders` → projected), most-recent-first.
    pub trust_history: Vec<TrustHistoryRow>,
    /// Which lens this row currently shows (per-row UI state; a `SetLens` action
    /// flips it without a nest round-trip).
    pub lens: TrustLens,
    /// This nest's content-processor holders a `Mint` can target (the
    /// `nest-trust-mint-holder-select` options) — empty ⇒ no mint affordance
    /// (falls back to `nest-trust-empty`'s "nothing to trust yet" framing). v1
    /// populates this only for the home row, same limitation as `trust_grants`.
    pub available_holders: Vec<AvailableHolder>,
    /// The mint picker's option catalog (`nest-trust-mint-scope-select`):
    /// which use cases the user can trust this nest with, pre-resolved to
    /// scope plus holder candidate(s) by the shared builder
    /// (`view_model::mint_options` — scope-first design, `nests.md` § Mint).
    /// Empty ⇒ no mint affordance (nothing derivable, or no content-processor
    /// holder enrolled). v1 populates this only for the home row, same
    /// limitation as `trust_grants`.
    pub mint_options: Vec<TrustMintOption>,
    /// Whether the user has blessed this nest (`nest-trust-blessed-toggle`;
    /// the account plane's `fauna.state.blessed-nests`, [`BlessedNestsStore`])
    /// — its standing grants renew themselves (`nests.md` § Expiry / renewal
    /// → *Duration and blessing*). v1 reads it
    /// for the home row only, like the rest of the trust facet; a pairing row
    /// is `false`.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    pub blessed: bool,
    /// The duration `nest-trust-mint-duration-select` pre-selects, and the one
    /// a `Mint` naming none takes: standard on a blessed nest, one-off
    /// otherwise (`fauna_client_capabilities::GrantDuration::default_for`).
    #[serde(default)]
    pub mint_default_duration: TrustGrantDuration,
    /// Trust facet — Now lens, **after** the content-processing grant rows: the
    /// backup powers this nest holds (`nest-trust-backup-item`;
    /// `fauna_client_backup::trust::backup_trust_rows` → projected). Populated
    /// only on the home row, and only when the machine carries
    /// [`trust::BackupTrustSeams`] — both backup grants empower the *source*
    /// nest, so they belong on no other row (`nests.md` § Trust facet — backup
    /// rows). Empty here does **not** mean `nest-trust-empty`: a home nest
    /// holding a backup row but zero content grants is not "trusted to read
    /// nothing" (`nests.md:99`).
    pub trust_backups: Vec<TrustBackupRow>,
    /// Trust facet — Now lens, **after** the backup trust rows: the retained
    /// backup generations each configured destination still holds inside the
    /// custody grace window `T` — what the owner can actually roll back to once
    /// they have revoked a rogue source (`nest-trust-generation-item`;
    /// `fauna_client_backup::generations::list_retained_generations` →
    /// flattened by [`trust::project_generation_rows`]). Populated only on the
    /// home row and only when the machine carries [`trust::BackupTrustSeams`],
    /// the same conditions as [`trust_backups`](Self::trust_backups).
    ///
    /// **Empty here means "every destination answered and none is retaining
    /// anything"** — the healthy steady state. A destination that could not be
    /// asked is present as an
    /// [`Unreachable`](trust::TrustGenerationStatus::Unreachable) row, never as
    /// an absence (`nests.md:122`).
    pub trust_generations: Vec<TrustGenerationRow>,
}

/// One content-processor holder a `Mint` action can target — the
/// `nest-trust-mint-holder-select` projection of [`trust::HolderInfo`] (key
/// material dropped; clients need only enough to label + address the pick).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AvailableHolder {
    /// The stable holder name `Mint.holder_bridge_id` names (e.g. `"mda"`,
    /// `"web-serve"`).
    pub bridge_id: String,
    /// The service-user role (`"mda"` / `"content-processor"`) — a display
    /// hint, not a targeting key (`bridge_id` is; role no longer discriminates
    /// between holders, see `LinkedNestsAction::Mint`).
    pub role: String,
}

/// What the Nests page's capabilities line shows for one pairing capability:
/// its user-voice i18n key where it has one — `account_replica`, the sealed
/// copy of the account plane a linked nest keeps (`linked-nests.md` § The
/// surface). A capability with no label degrades to its wire name (a key no
/// catalog holds resolves to itself) rather than vanishing: dropping one would
/// understate what the nest may sync. The same shape as the delegation row's
/// `delegation_capability_label`.
#[must_use]
pub fn capability_label(capability: &str) -> fauna_core::localized::LocalizedText {
    fauna_core::localized::LocalizedText::key(match capability {
        fauna_protocol::pair::capability::ACCOUNT_REPLICA => "nests.capability_account_replica",
        other => other,
    })
}

impl From<PairingRow> for LinkedNestRow {
    fn from(row: PairingRow) -> Self {
        Self {
            nest_id: to_hex(row.private_nest_id.as_ref()),
            capability_labels: row
                .capabilities
                .iter()
                .map(|c| capability_label(c))
                .collect(),
            capabilities: row.capabilities,
            expires_at: row.expires_at,
            created_at: row.created_at,
            label: row.label,
            nest_url: row.nest_url,
            // A linked (pairing) nest: not the home row, and v1 has no local
            // holder roster for it, so its trust facet is empty (`nest-trust-empty`).
            is_home: false,
            trust_grants: Vec::new(),
            trust_history: Vec::new(),
            lens: TrustLens::Now,
            available_holders: Vec::new(),
            mint_options: Vec::new(),
            blessed: false,
            mint_default_duration: TrustGrantDuration::default(),
            // Both backup grants empower the source (home) nest, so a pairing
            // row never carries one (`nests.md` § Trust facet — backup rows).
            trust_backups: Vec::new(),
            // Same reasoning: recovery is addressed to the *owner's* backup
            // destinations, which hang off the home row, not off a pairing.
            trust_generations: Vec::new(),
        }
    }
}

/// The user's own post-forward queue on the connected nest — the page-level
/// `nests-forward-queue` status (`ui/nests.md` § Forward queue; ruling:
/// `private-mode.md` § Post Forwarding → the queue is the user's to see).
/// Projected from the wire [`ForwardQueue`] that rides `fauna.pair.list`. A
/// shell renders it only while `queued > 0`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ForwardQueueStatus {
    /// Every entry the user has queued on this nest, backed-off ones included.
    pub queued: u64,
    /// Those refused past the retry ceiling (about 8.5 hours of backoff) and
    /// still retrying — the ones a relay-side grant is most likely missing for.
    pub stuck: u64,
    /// The most recent failure the nest's worker recorded, control-stripped;
    /// `None` until a send has failed (never `Some("")`). The "why" beside the
    /// count. Part of it is relay-chosen text, so a shell paints it as plain
    /// text only — never markup, never link-detected.
    pub last_error: Option<String>,
}

impl From<ForwardQueue> for ForwardQueueStatus {
    /// The one app-side strip of the relay-chosen reason (`private-mode.md`
    /// § Post Forwarding: the app renders it as untrusted text): every shell
    /// reads this projection, so no shell depends on the nest's own
    /// control-strip, and none needs a copy of this one.
    fn from(q: ForwardQueue) -> Self {
        let last_error = q
            .last_error
            .map(|e| fauna_core::control_chars::strip_control_chars(&e).into_owned())
            .filter(|e| !e.trim().is_empty());
        Self {
            queued: q.queued,
            stuck: q.stuck,
            last_error,
        }
    }
}

/// Read-only snapshot the per-app UI renders for `linked-nests`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LinkedNestsSnapshot {
    /// The connected/home nest row + its trust facet (`nests.md` § Layout — the
    /// home nest sits in the same list as pairings). `None` on a pairing-only
    /// machine (built without trust seams — existing clients, unchanged); `Some`
    /// once the trust facet is wired and hydrated. The per-app shell renders
    /// it first, then `pairings`.
    pub home: Option<LinkedNestRow>,
    /// The owner's active pairings (`fauna.pair.list`). Empty when none.
    pub pairings: Vec<LinkedNestRow>,
    pub status: LinkedNestStatus,
    /// Last action's error, surfaced via the `error-message` element. Includes
    /// the admin-policy rejection when pairing is disabled nest-wide.
    pub error: Option<String>,
    /// How the last
    /// [`RestoreGeneration`](LinkedNestsAction::RestoreGeneration) resolved, or
    /// `None` when the last action was not a restore. Typed rather than folded
    /// onto [`error`](Self::error) because
    /// [`PastRecoveryWindow`](trust::TrustRestoreOutcome::PastRecoveryWindow)
    /// is a **product state, not a failure** — see
    /// [`TrustRestoreOutcome`](trust::TrustRestoreOutcome). Each shell renders
    /// its own localized copy off this; only a genuinely failed call (transport
    /// refused, unknown destination) lands on `error`.
    ///
    /// Cleared at the start of every dispatch, so a stale "restored" can never
    /// sit on the page describing an action the user has since replaced.
    pub restore_outcome: Option<TrustRestoreOutcome>,
    /// The user's own post-forward queue on the connected nest, as the last
    /// `fauna.pair.list` reported it. `None` until the first refresh (an app then shows
    /// no queue at all). Rendered as the
    /// page-level `nests-forward-queue` status only while `queued > 0`.
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub forward_queue: Option<ForwardQueueStatus>,
}

impl LinkedNestsSnapshot {
    fn empty() -> Self {
        Self {
            home: None,
            pairings: Vec::new(),
            status: LinkedNestStatus::Idle,
            error: None,
            restore_outcome: None,
            forward_queue: None,
        }
    }
}

/// Actions the per-app UI dispatches. The add-form inputs ride
/// [`LinkedNestsAction::Link`] directly (write-only, like `fauna-client-dns`'s
/// `PutCredentials`) — there is no persistent form state in the snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum LinkedNestsAction {
    /// Re-read the owner's pairings (`fauna.pair.list`; page load / refresh).
    Refresh,
    /// Authorize one nest to sync the account (`fauna.pair.add`). `nest_id` is the
    /// nest's Ed25519 public key in hex (64 chars). Empty `capabilities` →
    /// the canonical full self-sync set ([`default_self_sync`]). This writes a
    /// single row on the **connected** nest — the out-of-band / single-end path
    /// (authorize a nest by identity without connecting to it).
    Link {
        nest_id: String,
        capabilities: Vec<String>,
        expires_at: Option<i64>,
        label: Option<String>,
        nest_url: Option<String>,
    },
    /// Link two of the user's own nests with **one action**, seeding the
    /// authorization row on *both* (`fauna.pair.add` to each). `other_nest_url`
    /// is the address of the other nest the user is also reachable on; the
    /// machine opens an authenticated connection to it (the user's same
    /// identity), discovers both nests' ids via `fauna.nest.info`, and writes the
    /// reciprocal rows: `{private_nest_id: other_id}` on the connected nest and
    /// `{private_nest_id: this_id}` on the other. This is the "log into 2+ nests,
    /// then pair them" UX that makes the asymmetric home-with-public-relay
    /// deployment work with no per-nest manual step (`docs/goal/behavior/
    /// linked-nests.md` § One action seeds both ends). Empty `capabilities` →
    /// [`default_self_sync`] (the relay grant — includes `mail_pull`).
    LinkBoth {
        other_nest_url: String,
        capabilities: Vec<String>,
        expires_at: Option<i64>,
        label: Option<String>,
    },
    /// Unlink a nest (`fauna.pair.revoke`). `nest_id` is the hex Ed25519 key.
    Unlink { nest_id: String },

    // ── forward queue (`nests-forward-*`, `ui/nests.md` § Forward queue) ──
    /// `nests-forward-retry-button` — re-arm every forward the user has
    /// queued on the connected nest for an immediate retry
    /// (`fauna.pair.forward_retry`), then re-list. The nudge for a user who
    /// granted forwarding on the relay's own side rather than by linking from
    /// here; correctness still rests on the nest's own retry
    /// (`private-mode.md` § Post Forwarding).
    RetryForwards,
    /// `nests-forward-discard-button` — drop every forward the user has queued
    /// on the connected nest (`fauna.pair.forward_discard`), then re-list. The
    /// posts themselves stay; only their relay leaves.
    DiscardForwards,

    // ── trust facet (Nests page) ────────────────────────────────────
    /// Trust ONE content-processor holder of the nest to read the given content:
    /// mint an HPKE-sealed grant to it (`mint_grant` + `fauna.capabilities.mint`)
    /// and append a signed `Mint` event to the log. `nest_id` names the row (v1:
    /// the home nest; a linked nest with no local holders errors). `holder_bridge_id`
    /// picks the target from `LinkedNestRow.available_holders` — Mint is
    /// deliberately single-holder (not "every content-processor on the nest"):
    /// a generic `content-processor` role now covers MULTIPLE distinct services
    /// (the MDA, the web-serve paywall holder, …), and the role alone doesn't
    /// discriminate which one a given `scope` is meant for (`nests.md` § Mint —
    /// the role is only the coarse method-allowlist gate; the scope is
    /// crypto-self-enforcing but says nothing about *which holder* should get
    /// it). Minting to every holder indiscriminately would hand each of them
    /// every scope the user ever grants any of them — an over-broad-grant bug,
    /// not a convenience. `scope` is the user's chosen content (e.g.
    /// `content.read{mail}`); "capability"/"grant" stay internal — the UI says
    /// "trust to read …" (`nests.md` § Naming).
    ///
    /// `duration` is the `nest-trust-mint-duration-select` pick; `None` takes
    /// the row's `mint_default_duration` (standard on a blessed nest, one-off
    /// otherwise — `nests.md` § Expiry / renewal → *Duration and blessing*).
    Mint {
        nest_id: String,
        holder_bridge_id: String,
        scope: Vec<TrustScope>,
        #[serde(default)]
        duration: Option<TrustGrantDuration>,
    },
    /// `nest-trust-blessed-toggle` — bless (or un-bless) a nest: its standing
    /// grants then renew themselves (`fauna.state.blessed-nests`, synced on the
    /// account plane — [`BlessedNestsStore`]).
    /// Blessing runs the renew sweep at once, through the refresh it ends with.
    SetBlessed { nest_id: String, blessed: bool },
    /// The auto-renew loop's tick — every app dispatches it at foreground and
    /// then every [`view_model::AUTO_RENEW_CHECK_SECS`] while running. Renews
    /// each blessed standing grant inside the renew-ahead threshold, silently
    /// and best-effort, then refreshes only if it renewed something. A Nests
    /// page `Refresh` runs the same sweep.
    AutoRenew,
    /// **One tap: trust this box with the default set** — the onboarding
    /// shortcut (`onboarding.md` § 3b-ter, the one ratified survivor of the
    /// retired claim-time trust question; `storage-modes.md` § What replaced
    /// each piece of the axis). Mints, in one gesture, **every option the
    /// shared mint catalog derives** ([`view_model::mint_options`] — the same
    /// list `nest-trust-mint-scope-select` offers), each to that option's own
    /// derived holder, at the standard [`DEFAULT_GRANT_WINDOW_SECS`] window.
    ///
    /// Three properties this deliberately keeps:
    /// * **No second availability rule.** The default set *is* the catalog, so
    ///   there is nothing that can drift away from what the picker offers.
    /// * **Still per-option holder derivation**, never a blanket mint to every
    ///   content processor — the over-broad-grant bug [`Self::Mint`]'s doc
    ///   describes is not reintroduced by minting several options at once.
    /// * **Idempotent**: a scope a live grant to that holder already covers is
    ///   skipped, so re-running the best-effort onboarding glue cannot leave
    ///   the user two capabilities to revoke where they granted one.
    ///
    /// An empty catalog (no content processor enrolled yet, mail not enabled)
    /// mints nothing and is **not** an error — the tap is an offer, and the
    /// last screen of onboarding must not paint a banner over a box that
    /// simply has nothing to be trusted with yet. Home nest only in v1, like
    /// every other trust action (hence no `nest_id`).
    MintDefaultSet,
    /// Renew a grant's window (`fauna.capabilities.renew` + a `Renew` event).
    /// `grant_id` is the 16-byte handle from the grant row.
    Renew { grant_id: Vec<u8> },
    /// Revoke a grant (`fauna.capabilities.revoke` + a `Revoke` event); the
    /// holder's next fetch goes dark. `grant_id` is the 16-byte handle.
    Revoke { grant_id: Vec<u8> },
    /// **Keep** a grant the post-succession aftermath carried across: clear its
    /// review mark and leave the grant itself untouched
    /// (`succession-aftermath.md` § Adjudicating what the aftermath carries
    /// across). `grant_id` is the 16-byte handle.
    ///
    /// The *Remove* half of the pair is deliberately absent here — it is
    /// [`Self::Revoke`] above, so no second revocation mechanism is minted.
    KeepGrant { grant_id: Vec<u8> },
    /// Flip a nest row's trust-facet lens (Now ⇄ History) — local UI state, no
    /// nest round-trip. `nest_id` names the row.
    SetLens { nest_id: String, lens: TrustLens },

    // ── backup trust rows (`nest-trust-backup-revoke`) ───────────────
    /// Freeze the source nest's ability to seal **new** segment backups
    /// (`fauna.backup.nest_key.revoke`, to the home nest). Held custody at each
    /// destination is untouched — the row's `nest-trust-backup-bound-note` says
    /// so. The `nest-trust-backup-revoke` press on the seal row.
    RevokeBackupSeal,
    /// Withdraw the source nest's writer authorization at ONE destination,
    /// spoken over that destination's own connection so it works with the source
    /// nest hostile. `destination_id` is the row's
    /// `BackupDestination::destination_id`. The `nest-trust-backup-revoke` press
    /// on a writer row.
    RevokeBackupWriter { destination_id: String },

    // ── generation recovery (`nest-trust-generation-restore`) ────────
    /// Roll one retained generation back to live at the destination holding it
    /// (`fauna.backup.generation.restore`), spoken over that destination's own
    /// connection so it works with the source nest hostile — the whole point of
    /// the surface (`nests.md` § Trust facet — generation recovery).
    ///
    /// The address triple comes verbatim off the
    /// [`TrustGenerationRow`] that was pressed; a shell never assembles one. It
    /// is deliberately **not** a row index: the rows are a flattened view over
    /// several destinations, and an index-addressed restore would silently
    /// promote the wrong generation the moment a shell filtered or re-ordered
    /// them. `destination_id` resolves to a URL through the client's own pinned
    /// `BackupState.backup.destinations`, never one the source supplied.
    RestoreGeneration {
        destination_id: String,
        folder_name: String,
        path_hash: String,
        manifest_hash: String,
    },
}

/// How the user-entered link-form value resolves — shared so every app routes
/// the same input to the same action (priority #2). A bare 64-hex Ed25519 key is
/// a nest **identity** (single-end [`LinkedNestsAction::Link`]); anything else is
/// treated as a nest **address** (both-ends [`LinkedNestsAction::LinkBoth`]),
/// validated when the connection is attempted.
// `Serialize` so the wasm `classifyLinkInput` export can cross the externally-
// tagged enum to the Svelte form (`{NestId:{nest_id}}` / `{NestUrl:{nest_url}}`),
// which routes it to the `Link` / `LinkBoth` dispatch — the shell-side classify
// that linux does in Rust (priority #2: one classifier, every app routes
// identically).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum LinkInput {
    /// A 64-char hex Ed25519 nest identity — authorize that one nest.
    NestId { nest_id: String },
    /// A nest address (URL) the user is also reachable on — link both ends.
    NestUrl { nest_url: String },
}

/// Classify a user-entered link-form value into the action it should drive. A
/// trimmed 64-char ASCII-hex string is a nest identity ([`LinkInput::NestId`] →
/// single-end `Link`, the existing out-of-band path); anything else is a nest
/// address ([`LinkInput::NestUrl`] → both-ends `LinkBoth`). The URL itself is
/// validated lazily when the second connection is opened, so a malformed address
/// surfaces as a connect error in the snapshot rather than a parse here.
pub fn classify_link_input(raw: &str) -> LinkInput {
    let s = raw.trim();
    if fauna_core::hex32::is_hex64(s) {
        LinkInput::NestId {
            nest_id: s.to_string(),
        }
    } else {
        LinkInput::NestUrl {
            nest_url: s.to_string(),
        }
    }
}

/// This nest's own identity + address, learned via `fauna.nest.info`
/// ([`LinkedNestsNest::this_nest`]). Used to seed the reciprocal pairing row on
/// the peer in a both-ends link, and as the `target_nest_id` every app's DNS
/// cert-issuance glue supplies to `DnsAction::IssueCert` / `BeginManualIssueCert`
/// (`tls-certificates.md` § C.3 — "the home nest the cert serves"; the single-nest
/// case is the connected nest itself). Carries the `uniffi::Record` derive so the
/// 5 native fan-out clients reach it through `fauna-ffi` exactly as linux reads it
/// directly.
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SelfNest {
    /// The nest's 32-byte Ed25519 public key.
    pub id: Vec<u8>,
    /// The nest's address (the URL the client is connected on).
    pub url: String,
}

/// WS-RPC seam to nest. Per-app glue implements this over the **bearer**
/// WS-RPC handle (`fauna.pair.{list,add,revoke}` are `User`-gated, owner-scoped
/// — no `actor_id` on the wire; the connection actor scopes every call).
//
// Native boxes `Send` futures (the seam may be driven by `tokio::spawn`); wasm's
// `Rc`-based `WsRpcClient` yields `!Send` futures, so it needs the `?Send` arm —
// exactly as `fauna-client-dns`'s `DnsNest`.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait LinkedNestsNest: MaybeSendSync {
    /// `fauna.pair.list` — the owner's active pairings, plus the owner's own
    /// post-forward queue on this nest (`forward_queue`).
    async fn list(&self) -> Result<PairListReply, PairNestError>;
    /// `fauna.pair.add` — authorize a pairing (owner-scoped write).
    async fn add(&self, req: PairAddRequest) -> Result<(), PairNestError>;
    /// `fauna.pair.revoke` — revoke a pairing by the linked nest's id.
    async fn revoke(&self, private_nest_id: Vec<u8>) -> Result<(), PairNestError>;
    /// `fauna.pair.forward_retry` — make every forward the owner has queued
    /// due now. Returns how many were re-armed.
    async fn forward_retry(&self) -> Result<u64, PairNestError>;
    /// `fauna.pair.forward_discard` — drop every forward the owner has
    /// queued. Returns how many left.
    async fn forward_discard(&self) -> Result<u64, PairNestError>;
    /// This nest's own identity + address, via the anonymous-discovery
    /// `fauna.nest.info` kind (callable on the authenticated connection — the
    /// pre-identity gate is skipped for authed conns). **The id is the nest's
    /// own claim, which nothing verified** — no decision keyed on "which nest
    /// is this" reads it on its own; every one goes through
    /// [`LinkedNestsMachine::bound_nest_id`], which compares it against
    /// [`Self::bound_nest_id`] and refuses a nest whose claim disagrees.
    async fn this_nest(&self) -> Result<SelfNest, PairNestError>;
    /// The `nest_actor_id` this **connection** is bound to — never the nest's
    /// own `fauna.nest.info` claim, which any box can answer with a sibling's
    /// id it learned from the pairing list. Every trust decision keyed on
    /// "which nest is this" reads it: the blessing that lets a box's grants
    /// renew without the user (`nests.md` § Expiry / renewal → *Duration and
    /// blessing*). The source is the pin the login graduated for this origin
    /// (`security.md` § Transport trust: pin the identity, never the cert) —
    /// on native TLS SPKI-compared, and the bearer connection is pinned to
    /// that SPKI; on web possession-verified at every login. Where no pin
    /// exists (a plaintext loopback nest) it is a possession proof over this
    /// connection, which is all a plaintext channel can bind.
    async fn bound_nest_id(&self) -> Result<Vec<u8>, PairNestError>;
    /// Open an authenticated connection to another of the user's nests (the
    /// user's *same* identity, registered on both) and return a seam over it, so
    /// the machine can write the reciprocal `fauna.pair.add` there. Native:
    /// `NestClient::new(url, keypair).connect()`. Web: a second `WsRpcClient`
    /// through the SPA-supplied [`PeerConnect`], its bearer minted on the
    /// peer origin's anonymous socket (`fauna-wasm`'s `make_peer_connect`);
    /// a web seam built without one returns [`PairNestError::Rejected`].
    async fn connect_peer(&self, peer_url: &str)
    -> Result<Arc<dyn LinkedNestsNest>, PairNestError>;
    /// The generations this nest holds escrow wraps for, for this account —
    /// one unfiltered `fauna.generation.escrow.get`. The unlink sweep's list
    /// (`account-sync-plane.md` § The bind leg, ruling 4).
    async fn escrow_generations(&self) -> Result<Vec<Vec<u8>>, PairNestError>;
    /// `fauna.generation.escrow.delete` — delete this account's wraps of one
    /// generation at this nest.
    async fn escrow_delete(&self, generation_id: Vec<u8>) -> Result<(), PairNestError>;
    /// The connection's own account's RecoveryKey registration chain as this
    /// nest serves it — the verbatim records, oldest first, empty when it
    /// holds none (`fauna.recovery.registration.chain`). One half of the
    /// both-ends link's chain reconcile
    /// (`fauna_client_core::recovery_chain`).
    async fn registration_chain(&self) -> Result<Vec<Vec<u8>>, PairNestError>;
    /// `fauna.recovery.registration.submit` of one verbatim record, for the
    /// connection's own account. The nest verifies it against the chain it
    /// holds; the client carries it and decides nothing.
    async fn submit_registration(&self, record: Vec<u8>) -> Result<(), PairNestError>;
    /// The succession statements that end at the connection's own account,
    /// verbatim and oldest hop first — `fauna.recovery.succession.status`'s
    /// `predecessor_statements`; empty for an identity that succeeded nobody.
    /// What the both-ends link delivers
    /// at the other nest before it connects there.
    async fn predecessor_statements(&self) -> Result<Vec<Vec<u8>>, PairNestError>;
    /// Submit a statement path at `nest_url` over an **anonymous** connection
    /// opened there for it
    /// (`fauna_client_core::succession_delivery::submit_statement_path`):
    /// the link's delivery, made before it signs in at that nest. Returns how
    /// many hops landed; an unreachable nest or a refused hop is the
    /// [`OwedReason`] the caller logs.
    async fn submit_succession_at(
        &self,
        nest_url: &str,
        path: Vec<Vec<u8>>,
    ) -> Result<usize, OwedReason>;

    // ── trust facet (Nests page) ────────────────────────────────────
    // The capability surface: discover a nest's content-processor holders (the
    // grant seal targets) and mint / renew / revoke content-processing grants
    // on it. `nests.md` § Where logic lives. v1 wires the connected nest's own
    // roster (the proven tier_3 flow); a peer seam from `connect_peer` inherits
    // these for the documented per-linked-nest follow-on.

    /// A nest's content-processor holders — `fauna.bridges.list_service_users`
    /// (approved, `has_x25519`, content-processor role) resolved to X25519 seal
    /// targets via `fauna.bridges.fetch_bridge_pubkey` (`nests.md:106`). A nest
    /// with none renders the `nest-trust-empty` state.
    async fn content_processor_holders(&self) -> Result<Vec<HolderInfo>, PairNestError>;
    /// `fauna.capabilities.mint` — deposit an HPKE-sealed `GrantBlob` (its
    /// canonical bytes). The owner never reads it back (holder-scoped fetch).
    async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), PairNestError>;
    /// `fauna.capabilities.renew` — slide a grant's window to
    /// `[new_epoch_start, new_epoch_end]` (`grant_log::renewal_window`: one
    /// mint-length behind the renewal instant, one ahead; the nest prunes the
    /// per-epoch wraps below the new start). A master-key grant renews as a
    /// window move only (`appended_keys` empty); a **bounded** mail grant's
    /// renewal carries one canonical
    /// [`fauna_mls::wrapped_blob::WrappedScopeKey`] per sealing epoch the
    /// extension newly covers (`bounded_mail_renewal_keys`, computed by the
    /// machine — the nest refuses a bounded extension that leaves an epoch
    /// uncovered). The same shape as `fauna-client-mail-settings`' heal seam.
    async fn renew_grant(
        &self,
        grant_id: [u8; 16],
        new_epoch_start: u64,
        new_epoch_end: u64,
        appended_keys: Vec<Vec<u8>>,
    ) -> Result<(), PairNestError>;
    /// `fauna.capabilities.revoke` — delete the `(owner, grant_id)` row; the
    /// holder's next fetch goes dark (honest box).
    async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), PairNestError>;
    /// `fauna.capabilities.reconcile` — every `grant_id` this owner holds on
    /// this nest (all rows, expired included). The **only** consumer is
    /// [`LinkedNestsMachine`]'s reconcile sweep; nothing from the answer may
    /// reach the trust facet's lenses (`ui/nests.md` § Trust facet — grants →
    /// *Reconcile*).
    async fn reconcile_grants(&self) -> Result<Vec<[u8; 16]>, PairNestError>;
}

// `CONTENT_PROCESSOR_ROLES`, `is_content_processor_holder`, and
// `discover_holders` moved to `fauna-client-bridges` (2026-07-19) — the home of
// `MailAdminClient` — so the mail-settings rotation-heal driver shares the ONE
// discovery implementation (priority #2). `discover_holders` is imported above;
// its `DiscoverHoldersError` *is* `PairNestError` since 2026-08-23 (both are
// `fauna_protocol::NestSeamError`), so the seam impls' `content_processor_holders`
// methods keep their `PairNestError` signature with no conversion at all.

/// A generic post-link side effect fired once a both-ends [`LinkBoth`] has
/// seeded the reciprocal pairing rows. The pairing crate carries **no** mail (or
/// any other consumer's) dependency — the hook takes only the just-linked peer's
/// address; the impl lives in the consumer crate and is wired once at machine
/// construction ([`LinkedNestsMachine::with_post_link`]).
///
/// First consumer: mail. After a user links their home/relay box, the mail impl
/// (`fauna-client-mail-settings`) auto-provisions the user's mailbox onto the
/// just-linked peer (the home box), reusing the fleet MSEK — the one-action
/// home-with-public-relay flow (`docs/goal/architecture/nest/
/// deployment-home-with-public-relay.md` § Pairing;
/// `docs/goal/behavior/mail-credentials.md` § Trigger taxonomy). The doc names
/// Nostr bridging as the next planned consumer of the same pairing + relay
/// machinery, so the seam is deliberately generic rather than mail-specific.
///
/// `Send` arms mirror [`LinkedNestsNest`]: native boxes `Send` futures, wasm's
/// `Rc`-based transport yields `!Send` futures.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait PostLinkHook: MaybeSendSync {
    /// Called after [`LinkedNestsMachine`]'s `LinkBoth` has written **both**
    /// reciprocal `fauna.pair.add` rows and re-listed. `peer_url` is the
    /// just-linked nest's address. An `Err` surfaces in the snapshot but does
    /// **not** undo the link (the rows are durable + re-running `LinkBoth` is
    /// idempotent); a no-op `Ok(())` is the right answer when there is nothing to
    /// do (e.g. mail isn't enabled, so there is no mailbox to provision).
    async fn after_link_both(&self, peer_url: &str) -> Result<(), PairDispatchError>;
}

// ── machine ─────────────────────────────────────────────────────────

struct Inner {
    snapshot: LinkedNestsSnapshot,
}

/// One instance per client. Holds the rendered snapshot; drives the seam.
/// Mirrors `fauna-client-dns`'s `DnsManagementMachine`.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct LinkedNestsMachine {
    nest: Arc<dyn LinkedNestsNest>,
    /// Optional side effect run after a both-ends `LinkBoth` seeds the rows
    /// ([`PostLinkHook`]). `None` keeps pure pairing behavior (single-end clients;
    /// web, which dispatches `LinkBoth` but builds its machine with no hook yet).
    post_link: Option<Arc<dyn PostLinkHook>>,
    /// Trust-facet seams (Nests page). `None` ⇒ a pairing-only machine (existing
    /// clients / constructors, behavior unchanged: no `home` row, empty trust,
    /// and a trust action errors `InvalidState`). `Some` ⇒ the machine hydrates
    /// the home row's trust facet and drives Mint/Renew/Revoke. See
    /// [`trust::TrustSeams`].
    trust: Option<TrustSeams>,
    inner: Mutex<Inner>,
}

// `new` takes an `Arc<dyn …>` seam (not an FFI type), so it stays in a plain
// (non-exported) impl alongside the private helpers; the FFI surface —
// `snapshot` (sync) + `hydrate`/`dispatch` (async) — lives in the exported impl
// blocks below. Mirrors `DnsManagementMachine`'s layout.
impl LinkedNestsMachine {
    pub fn new(nest: Arc<dyn LinkedNestsNest>) -> Self {
        Self {
            nest,
            post_link: None,
            trust: None,
            inner: Mutex::new(Inner {
                snapshot: LinkedNestsSnapshot::empty(),
            }),
        }
    }

    /// Like [`new`](Self::new) but wires a [`PostLinkHook`] fired after every
    /// both-ends `LinkBoth`. The consumer crate (mail) constructs the hook and
    /// passes it here once at machine construction (the shared `build_*` path),
    /// so the trigger is identical on every app (priority #2).
    pub fn with_post_link(
        nest: Arc<dyn LinkedNestsNest>,
        post_link: Arc<dyn PostLinkHook>,
    ) -> Self {
        Self {
            nest,
            post_link: Some(post_link),
            trust: None,
            inner: Mutex::new(Inner {
                snapshot: LinkedNestsSnapshot::empty(),
            }),
        }
    }

    /// Like [`new`](Self::new) but wires the trust-facet seams (Nests page):
    /// the config store (grant-event log persistence), the grant-event signer
    /// (raw key behind the seam), and the platform clock/randomness. A machine
    /// built this way hydrates the home nest's trust facet and drives
    /// Mint/Renew/Revoke; the pairing surface is unchanged.
    pub fn new_with_trust(nest: Arc<dyn LinkedNestsNest>, trust: TrustSeams) -> Self {
        Self {
            nest,
            post_link: None,
            trust: Some(trust),
            inner: Mutex::new(Inner {
                snapshot: LinkedNestsSnapshot::empty(),
            }),
        }
    }

    /// Wire the seam that names a folder grant's folder on the trust facet
    /// ([`TrustFolderNames`]). A machine built without trust seams has no
    /// facet to name anything on, so this is then a no-op.
    #[must_use]
    pub fn with_folder_names(mut self, names: TrustFolderNames) -> Self {
        if let Some(trust) = self.trust.as_mut() {
            trust.folder_names = Some(names);
        }
        self
    }

    /// The full-fat constructor: both a [`PostLinkHook`] and the trust-facet
    /// seams. The mail consumer uses this so a client gets the mailbox
    /// auto-provision hook *and* the Nests-page trust facet from one machine.
    pub fn with_post_link_and_trust(
        nest: Arc<dyn LinkedNestsNest>,
        post_link: Arc<dyn PostLinkHook>,
        trust: TrustSeams,
    ) -> Self {
        Self {
            nest,
            post_link: Some(post_link),
            trust: Some(trust),
            inner: Mutex::new(Inner {
                snapshot: LinkedNestsSnapshot::empty(),
            }),
        }
    }

    fn set_status(&self, status: LinkedNestStatus) {
        self.inner.lock().expect("snapshot mutex").snapshot.status = status;
    }

    async fn refresh(&self) -> Result<(), PairDispatchError> {
        self.refresh_with(true).await
    }

    /// [`Self::refresh`], with the auto-renew sweep optional — `AutoRenew`
    /// refreshes after a sweep of its own and must not sweep twice.
    async fn refresh_with(&self, renew_sweep: bool) -> Result<(), PairDispatchError> {
        self.set_status(LinkedNestStatus::Loading);
        let listed = self.nest.list().await?;
        let pairings = listed.pairings;
        let forward_queue = Some(ForwardQueueStatus::from(listed.forward_queue));
        // Trust facet for the connected/home nest (v1 scope, `nests.md:110`):
        // discover its content-processor holders + fold the grant log Now/History.
        // Preserve the row's current lens across a refresh (local UI state that a
        // nest round-trip must not reset).
        let home = if self.trust.is_some() {
            let prior_lens = self
                .inner
                .lock()
                .expect("snapshot mutex")
                .snapshot
                .home
                .as_ref()
                .map(|h| h.lens)
                .unwrap_or_default();
            // Enforce the log-most-permissive invariant against this nest BEFORE
            // the row is built (`nests.md` § Trust facet — grants → *Reconcile*).
            // Nothing it does can change what `build_home_row` renders — the
            // sweep appends no event and writes no config — so its only ordering
            // constraint is the one inside it: enumerate, then read the config.
            self.reconcile_sweep().await;
            // The Nests page open is the app in the foreground on it: renew
            // what is due before the row is built, so it shows the result.
            if renew_sweep {
                self.auto_renew_sweep().await;
            }
            let mut row = self.build_home_row().await?;
            row.lens = prior_lens;
            Some(row)
        } else {
            None
        };
        let mut inner = self.inner.lock().expect("snapshot mutex");
        inner.snapshot.pairings = pairings.into_iter().map(LinkedNestRow::from).collect();
        inner.snapshot.home = home;
        inner.snapshot.forward_queue = forward_queue;
        inner.snapshot.status = LinkedNestStatus::Idle;
        Ok(())
    }

    /// `RetryForwards`: re-arm the user's queued forwards, then re-list so the
    /// page shows the queue as the nest now reports it. The nest's worker does
    /// the sending on its next pass — this only makes the entries due.
    async fn retry_forwards(&self) -> Result<(), PairDispatchError> {
        self.set_status(LinkedNestStatus::Working);
        self.nest.forward_retry().await?;
        self.refresh().await
    }

    /// `DiscardForwards`: drop the user's queued forwards, then re-list (the
    /// queue reads empty once the nest confirms it).
    async fn discard_forwards(&self) -> Result<(), PairDispatchError> {
        self.set_status(LinkedNestStatus::Working);
        self.nest.forward_discard().await?;
        self.refresh().await
    }

    /// **The reconcile sweep** — revoke, on the answering nest, every grant row
    /// the owner's own log does not hold live (`docs/goal/ui/nests.md` § Trust
    /// facet — grants → *Reconcile*, ratified 2026-08-15, which owns every
    /// constraint below and none of which may be relaxed).
    ///
    /// This is the retroactive enforcement of the invariant *the log is the more
    /// permissive of the pair*: it reaches rows stranded before record-then-
    /// deposit landed, and any row a hostile or buggy nest resurrects after a
    /// revoke. It resolves every client/nest disagreement in the one safe
    /// direction — narrow the nest — never the forbidden one, widening the
    /// client's view from a nest read.
    ///
    /// **The order is the correctness argument, not a style choice.** Enumerate
    /// *first*, then load the config: record-then-deposit guarantees an honest
    /// deposit's `Mint` is durable **before** its row exists, so a config read
    /// taken after the enumerate can only be *newer* than the row list, which
    /// makes an honest-nest false orphan structurally impossible. Reading the
    /// config first would reintroduce exactly the window that ordering closes —
    /// a grant minted between the two reads would look like an orphan and be
    /// revoked out from under the user.
    ///
    /// **Silent and best-effort by ratified constraint.** An orphan is invisible
    /// by construction, so no user expectation can depend on it and there is no
    /// UI surface, no confirm step, and no error to raise: every failure path
    /// degrades to doing less rather than failing the page. A refusal or
    /// transport fault on the enumerate is simply left alone; one failing revoke does not
    /// abandon the rest, because each is independent and the next refresh
    /// retries. Returns how many revokes it landed, which is local diagnostics
    /// only — the log and both lenses are untouched.
    async fn reconcile_sweep(&self) -> usize {
        let Ok(seams) = self.trust_seams() else {
            return 0;
        };
        // 1. Enumerate. Any refusal or transport fault ends the sweep here: the
        //    orphans stay exactly as unreachable as they already were.
        let Ok(nest_ids) = self.nest.reconcile_grants().await else {
            return 0;
        };
        if nest_ids.is_empty() {
            return 0;
        }
        // 2. Read the log fresh, strictly after the enumerate.
        let Ok(ledger) = seams.ledger.load().await else {
            return 0;
        };
        // 3. Judge against the log alone, bounded by the per-owner cap, and
        //    revoke what it does not recognize — back to this same nest, and
        //    with no `GrantEvent` recorded (the ledger is dropped unmodified: a
        //    `Revoke` event for an id the log never minted would be
        //    nest-influenced content entering the signed log).
        let mut revoked = 0usize;
        for grant_id in grant_log::unrecognized_grant_ids(&ledger, nest_ids) {
            if self.nest.revoke_grant(grant_id).await.is_ok() {
                revoked += 1;
            }
        }
        revoked
    }

    /// Whether the user has blessed `nest_id` ([`BlessedNestsStore`]). An
    /// unreadable verdict — no account runtime yet, a store refusal, an id
    /// that is not 32 bytes — reads as un-blessed: the restrictive answer, so
    /// nothing renews in the background on a verdict nobody could read.
    async fn blessing_of(&self, seams: &TrustSeams, nest_id: &[u8]) -> bool {
        let Ok(id) = <[u8; 32]>::try_from(nest_id) else {
            return false;
        };
        match seams.blessings.is_blessed(&id).await {
            Ok(blessed) => blessed,
            Err(e) => {
                tracing::warn!(target: "fauna_pair", "blessing unreadable, rendered un-blessed: {e}");
                false
            }
        }
    }

    /// Record the user's blessing verdict for `nest_id` at `now`
    /// ([`BlessedNestsStore::set_blessed`]); whether anything changed.
    async fn bless(
        &self,
        seams: &TrustSeams,
        nest_id: &[u8],
        blessed: bool,
        now: u64,
    ) -> Result<bool, PairDispatchError> {
        let id = <[u8; 32]>::try_from(nest_id).map_err(|_| {
            PairDispatchError::InvalidState(format!(
                "a nest identity is 32 bytes; got {}",
                nest_id.len()
            ))
        })?;
        seams
            .blessings
            .set_blessed(&id, blessed, now)
            .await
            .map_err(|e| PairDispatchError::InvalidState(format!("blessing not recorded: {e}")))
    }

    /// The trust-facet seams, or an `InvalidState` error naming the pairing-only
    /// build — every trust action goes through this so a machine built without
    /// them fails cleanly instead of panicking.
    fn trust_seams(&self) -> Result<&TrustSeams, PairDispatchError> {
        self.trust.as_ref().ok_or_else(|| {
            PairDispatchError::InvalidState(
                "trust facet unavailable (machine built without trust seams)".into(),
            )
        })
    }

    /// Build the connected/home nest's row + trust facet: its identity (via
    /// `fauna.nest.info`), its content-processor holder set, and the Now/History
    /// folds of the grant log filtered to those holders (`nests.md` § Where logic
    /// lives). A blessed home nest's holders are the blessed set the liveness
    /// fold reads (`nests.md` § Expiry / renewal → *Duration and blessing*).
    async fn build_home_row(&self) -> Result<LinkedNestRow, PairDispatchError> {
        let seams = self.trust_seams()?;
        let this = self.nest.this_nest().await?;
        // The row names — and its blessing toggle writes — the identity this
        // connection proved, never `this.id` (the nest's own claim).
        let bound = self.nest.bound_nest_id().await?;
        // Holder discovery rides `fauna.bridges.list_service_users`, which is
        // Admin-gated on the nest (`nests.md` § Implementation status — the trust
        // facet is admin-scoped in v1). A non-admin owner still gets their home
        // nest row (identity via the ungated `fauna.nest.info` in `this_nest`),
        // just with an empty trust facet — degrade to no holders rather than
        // hard-failing the whole `refresh` and blanking the Nests page for every
        // non-admin user (pairings are set only *after* the home row builds). An
        // empty holder set folds to the honest `nest-trust-empty` state: no grant
        // we can enumerate reaches this nest.
        let holder_infos = self
            .nest
            .content_processor_holders()
            .await
            .unwrap_or_default();
        let holders: BTreeSet<Vec<u8>> = holder_infos.iter().map(|h| h.pubkey.to_vec()).collect();
        // The grant log and its marks are the succession ledger's, which lives
        // in the account store. A store still assembling after sign-in renders
        // the facet empty — the non-admin degrade above — so the page still
        // lists and links; every gesture that writes the log refuses on its own
        // load until the store is up, and the next refresh fills the facet. A
        // store that answered and failed still fails the refresh.
        let ledger = match seams.ledger.load().await {
            Ok(ledger) => ledger,
            Err(e) if e.is_not_ready() => {
                tracing::warn!(target: "fauna_pair", "trust facet rendered empty: {e}");
                SuccessionLedger::empty(ActorId(seams.actor_id))
            }
            Err(e) => return Err(e.into()),
        };
        // The liveness fold reads the trust facet's RENDER clock — the real
        // clock plus an e2e offset that is zero in every real run
        // (`trust_clock`); mint/renew below stamp windows from the real one.
        let now = fauna_client_capabilities::trust_clock::render_now_secs(
            seams.platform.now_epoch_secs(),
        );
        let blessed = self.blessing_of(seams, &bound).await;
        let blessed_holders = if blessed {
            holders.clone()
        } else {
            BTreeSet::new()
        };
        let facet = view_model::trust_facet_for_holders(&ledger, &holders, now, &blessed_holders);
        let history = view_model::history_for_holders(&ledger, &holders);
        // The web-serve paywall grant's folder, joined here once so no shell
        // re-derives it; unreadable set names name no folder this refresh.
        let folders = match &seams.folder_names {
            Some(names) => names.resolve(&ledger.grant_events).await,
            None => None,
        };
        let mint_holders: Vec<view_model::MintHolder> = holder_infos
            .iter()
            .map(|h| view_model::MintHolder {
                bridge_id: h.bridge_id.clone(),
                role: h.role.clone(),
            })
            .collect();
        // A mail custody that cannot be read offers no mail/calendar option
        // rather than failing the facet (the options probe derivability).
        let mail = seams.mail.load().await.unwrap_or_default();
        // An unreadable custody (the account store not up yet) offers no post
        // tier this refresh — the picker degrades, it never guesses a key.
        let period_keys = seams.period_keys.custody().await.unwrap_or_default();
        let mint_options = view_model::mint_options(&period_keys, &mail, &mint_holders)
            .into_iter()
            .map(trust::project_mint_option)
            .collect();
        // Backup trust rows. Degrade to none on a read failure rather than
        // failing the whole refresh: the backup facet must never be able to
        // blank the Nests page, exactly as the admin-gated holder discovery
        // above degrades for a non-admin owner. The writer rows key on
        // `bound` — a source claiming a sibling's id must not be shown (or
        // offered for revoke) the sibling's grant.
        // The destination list is the bound box's `fauna.state.backup` row —
        // never another box's (`backup-destinations.md` § *Destination data
        // model*); an unreadable store renders no backup rows, as below.
        let destinations = match (&seams.backup, bound_array(&bound)) {
            (Some(b), Some(box_id)) => b
                .state
                .backup_state(box_id)
                .await
                .map(|state| state.backup.destinations)
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let trust_backups = match &seams.backup {
            Some(b) => fauna_client_backup::trust::backup_trust_rows(
                b.source.as_ref(),
                &to_hex(&bound),
                &destinations,
                b.connector.as_ref(),
            )
            .await
            .unwrap_or_default()
            .into_iter()
            .map(trust::project_backup_row)
            .collect(),
            None => Vec::new(),
        };
        // Retained generations, read over each destination's OWN connection —
        // no source seam is passed because none exists on this API, so
        // "accidentally ask the source" is not expressible here. Unlike the
        // rows above there is no `unwrap_or_default()`: the shared read never
        // fails wholesale, degrading per destination to an `Unreachable` group
        // instead, which is precisely the distinction the facet must preserve
        // (`nests.md:122`). Collapsing that to `Vec::new()` on error would
        // reintroduce the "could not ask" → "nothing to recover" lie.
        let trust_generations = match &seams.backup {
            Some(b) => trust::project_generation_rows(
                fauna_client_backup::generations::list_retained_generations(
                    &destinations,
                    b.connector.as_ref(),
                )
                .await,
            ),
            None => Vec::new(),
        };
        Ok(LinkedNestRow {
            nest_id: to_hex(&bound),
            // The home nest is the user's own — no per-pairing sync caps, no
            // expiry, no Unlink; the shell keys those off `is_home`.
            capabilities: Vec::new(),
            capability_labels: Vec::new(),
            expires_at: None,
            created_at: 0,
            label: None,
            nest_url: Some(this.url),
            is_home: true,
            trust_grants: facet
                .grants
                .into_iter()
                .map(|g| trust::project_grant(g, &ledger.unattested_grant_marks, folders.as_ref()))
                .collect(),
            trust_history: history
                .into_iter()
                .map(|h| trust::project_history(h, folders.as_ref()))
                .collect(),
            lens: TrustLens::Now,
            available_holders: holder_infos
                .iter()
                .map(|h| AvailableHolder {
                    bridge_id: h.bridge_id.clone(),
                    role: h.role.clone(),
                })
                .collect(),
            mint_options,
            blessed,
            mint_default_duration: fauna_client_capabilities::GrantDuration::default_for(blessed)
                .into(),
            trust_backups,
            trust_generations,
        })
    }

    /// The backup-facet seams, or an `InvalidState` error — mirrors
    /// [`trust_seams`](Self::trust_seams) one level down, so a client that wired
    /// the trust facet but not the backup half fails cleanly.
    fn backup_seams(&self) -> Result<&trust::BackupTrustSeams, PairDispatchError> {
        self.trust_seams()?.backup.as_ref().ok_or_else(|| {
            PairDispatchError::InvalidState(
                "backup trust rows unavailable (machine built without backup seams)".into(),
            )
        })
    }

    /// Freeze the source nest's ability to seal **new** segment backups for this
    /// owner (`fauna.backup.nest_key.revoke`, spoken to the home nest). Custody
    /// already held at each destination is untouched — which is exactly what the
    /// row's required `nest-trust-backup-bound-note` copy tells the user.
    async fn revoke_backup_seal(&self) -> Result<(), PairDispatchError> {
        let backup = self.backup_seams()?;
        self.set_status(LinkedNestStatus::Working);
        fauna_client_backup::trust::revoke_backup_seal(backup.source.as_ref())
            .await
            .map_err(PairDispatchError::InvalidState)?;
        self.refresh().await
    }

    /// Withdraw the source nest's authorization to write this owner's backup
    /// custody at one destination, spoken over **the destination's own**
    /// authenticated connection (`fauna.backup.writer_grant.revoke`).
    ///
    /// Routing it to the destination rather than through the source nest is what
    /// makes the affordance work when the source nest is the thing you are
    /// revoking *because of*: a hostile source can neither swallow the call nor
    /// lie about the result. The destination URL comes from the client's own
    /// pinned `BackupState.backup.destinations`, never from the source — and
    /// the writer it names is the id this connection **proved**
    /// ([`LinkedNestsNest::bound_nest_id`]), never the source's own
    /// `fauna.nest.info` claim, which could name a sibling and have the user
    /// delete that sibling's grant while the liar's survives. Deliberately not
    /// [`bound_nest_id`](Self::bound_nest_id)'s refuse-on-disagreement: a lying
    /// source is the case this affordance exists for, so its lie must not be
    /// able to block its own revocation.
    ///
    /// A destination answering "no such grant" while it still lists the grant
    /// is a failed revoke and errors; one whose list confirms the grant gone
    /// had nothing to revoke, and the refresh shows the row `missing`.
    async fn revoke_backup_writer(&self, destination_id: String) -> Result<(), PairDispatchError> {
        let backup = self.backup_seams()?;
        self.set_status(LinkedNestStatus::Working);

        let bound = self.nest.bound_nest_id().await?;
        let destinations = backup_destinations_of(backup, &bound).await?;
        let Some(dest) = destinations
            .iter()
            .find(|d| d.destination_id == destination_id)
        else {
            return Err(PairDispatchError::InvalidState(format!(
                "no backup destination {destination_id:?} in this box's list"
            )));
        };
        let writer_id = to_hex(&bound);
        let outcome = fauna_client_backup::trust::revoke_backup_writer(
            backup.connector.as_ref(),
            dest,
            &writer_id,
        )
        .await
        .map_err(PairDispatchError::InvalidState)?;
        if outcome == fauna_client_backup::trust::WriterRevokeOutcome::NothingToRevoke {
            tracing::info!(
                target: "fauna_pair",
                "backup writer revoke: the destination held no grant for this nest"
            );
        }
        self.refresh().await
    }

    /// Roll one retained generation back to live at the destination holding it
    /// (`fauna.backup.generation.restore`), over **the destination's own**
    /// authenticated connection.
    ///
    /// Same routing argument as [`revoke_backup_writer`](Self::revoke_backup_writer),
    /// and for the same reason one step further along: revoking freezes the
    /// damage, this is what undoes it. Both must work with the source nest fully
    /// hostile, so the URL is resolved from the client's own pinned
    /// `BackupState.backup.destinations` and the call is spoken to that box —
    /// never through the source, which is the writer being recovered from.
    ///
    /// A [`RestoreOutcome::NoSuchGeneration`] is **not** an error: it means the
    /// generation is unknown or already reclaimed past `T`. It lands on
    /// [`LinkedNestsSnapshot::restore_outcome`] as
    /// [`TrustRestoreOutcome::PastRecoveryWindow`] so the shell says "past the
    /// recovery window" rather than reporting a broken restore (`nests.md:124`).
    async fn restore_generation(
        &self,
        destination_id: String,
        folder_name: String,
        path_hash: String,
        manifest_hash: String,
    ) -> Result<(), PairDispatchError> {
        let backup = self.backup_seams()?;
        self.set_status(LinkedNestStatus::Working);

        let bound = self.nest.bound_nest_id().await?;
        let destinations = backup_destinations_of(backup, &bound).await?;
        let Some(dest) = destinations
            .iter()
            .find(|d| d.destination_id == destination_id)
        else {
            return Err(PairDispatchError::InvalidState(format!(
                "no backup destination {destination_id:?} in this box's list"
            )));
        };
        // The address triple round-trips from the row unchanged; only the three
        // fields the wire needs are carried, so nothing a shell displays can
        // influence *which* generation is promoted.
        let generation = fauna_client_backup::generations::RetainedGeneration {
            folder_name,
            path: None,
            path_hash,
            manifest_hash,
            size_bytes: 0,
            superseded_at: 0,
            expires_at: 0,
        };
        let outcome = fauna_client_backup::generations::restore_generation(
            backup.connector.as_ref(),
            dest,
            &generation,
        )
        .await
        .map_err(PairDispatchError::InvalidState)?;
        let refreshed = self.refresh().await;
        // Stamped AFTER the refresh, which rebuilds the snapshot wholesale —
        // setting it first would hand the fresh snapshot a `None` and lose the
        // one thing the user pressed for.
        self.inner
            .lock()
            .expect("snapshot mutex")
            .snapshot
            .restore_outcome = Some(match outcome {
            fauna_client_backup::generations::RestoreOutcome::Restored => {
                TrustRestoreOutcome::Restored
            }
            fauna_client_backup::generations::RestoreOutcome::NoSuchGeneration => {
                TrustRestoreOutcome::PastRecoveryWindow
            }
        });
        refreshed
    }

    /// Mint a content-processing grant to ONE content-processor holder of the
    /// named nest (v1: the connected nest), picked by `holder_bridge_id`: derive
    /// the minimal per-scope key + seal a `GrantBlob` (`mint_grant`), record a
    /// signed `Mint` event (seam-signed — the raw key stays behind the signer
    /// seam) through the succession-ledger seam, and only **then**
    /// deposit the blob (`fauna.capabilities.mint`) — the record-then-deposit
    /// order [`grant_log::UndepositedGrant`] enforces. Finally re-lists.
    /// `nest_id` is accepted
    /// for forward-compat (per-nest addressing) but v1 always targets the
    /// connected nest's roster. **Deliberately single-holder** — see
    /// [`LinkedNestsAction::Mint`] for why "one grant per content-processor
    /// holder" (the pre-`content-processor`-role behavior) is an over-broad-grant
    /// bug now that the role covers more than the MDA.
    async fn mint(
        &self,
        nest_id: String,
        holder_bridge_id: String,
        scope: Vec<TrustScope>,
        duration: Option<TrustGrantDuration>,
    ) -> Result<(), PairDispatchError> {
        let seams = self.trust_seams()?;
        self.set_status(LinkedNestStatus::Working);

        let holders = self.nest.content_processor_holders().await?;
        let Some(holder) = holders.iter().find(|h| h.bridge_id == holder_bridge_id) else {
            return Err(PairDispatchError::InvalidState(format!(
                "no content processor {holder_bridge_id:?} on this nest"
            )));
        };
        let scope_tuples: Vec<ScopeTuple> = scope.iter().map(trust::scope_tuple_from).collect();
        let event_scopes: Vec<_> = scope.iter().map(trust::grant_event_scope_from).collect();

        let mail = seams.mail.load().await?;
        let now = seams.platform.now_epoch_secs();
        let duration: fauna_client_capabilities::GrantDuration = match duration {
            Some(d) => d.into(),
            None => fauna_client_capabilities::GrantDuration::default_for(
                self.blessing_of(seams, &parse_nest_id(&nest_id)?).await,
            ),
        };
        let window_end = now + duration.secs();
        let grant_id = seams.platform.new_grant_id();
        // Unreadable ⇒ `None`: a post tier then refuses as unread.
        let period_keys = seams.period_keys.custody().await.ok();
        let blob = mint_grant(
            period_keys.as_ref(),
            &mail,
            &seams.actor_id,
            &grant_id,
            &holder.pubkey,
            holder.mlkem_ek.as_deref(),
            GrantWindow(now, window_end),
            &scope_tuples,
        )?;
        let blob_bytes = blob.to_canonical_bytes()?;
        let pending = grant_log::UndepositedGrant::new(grant_id, blob_bytes);

        let unsigned = grant_log::build_mint_event(
            grant_id,
            holder.pubkey,
            event_scopes,
            now,
            window_end,
            now,
        );
        let signed = seams.signer.sign_grant_event(unsigned)?;
        // Record-then-deposit, same rule as the batch above: at N=1 the window
        // is one refused ledger write, and the orphan it strands is just as
        // undiscoverable.
        let stored = seams
            .ledger
            .merge(SuccessionLedger::events_replica(
                ActorId(seams.actor_id),
                vec![signed],
            ))
            .await?;
        let recorded = grant_log::RecordedGrants::from_stored(&stored);
        let deposited = match pending.release(&recorded) {
            Ok(blob_bytes) => self.nest.mint_grant(blob_bytes).await.map_err(Into::into),
            Err(e) => Err(PairDispatchError::from(e)),
        };
        let refreshed = self.refresh().await;
        deposited.and(refreshed)
    }

    /// One-tap "trust this box" — mint the whole derivable default set
    /// ([`LinkedNestsAction::MintDefaultSet`], `onboarding.md` § 3b-ter).
    ///
    /// Reads the catalog from ONE config load and the already-held grants from
    /// ONE ledger read, and records every new event in ONE ledger merge: the
    /// alternative (dispatching [`Self::mint`] per option) would run N
    /// read/write round trips for one gesture.
    ///
    /// The batch is **not** atomic with the nest and cannot be — the deposit is
    /// a separate machine's commit point. What holds instead is the achievable
    /// invariant: the whole log is durable before the first blob ships, so
    /// whatever the nest ends up holding, the user's app can see and revoke it
    /// ([`grant_log::UndepositedGrant`]). Batching is what once made this
    /// dangerous — it widened the unrecorded window from one grant to N — and it
    /// is safe now only because of the ordering, not despite it.
    async fn mint_default_set(&self) -> Result<(), PairDispatchError> {
        let seams = self.trust_seams()?;
        self.set_status(LinkedNestStatus::Working);

        let holders = self.nest.content_processor_holders().await?;
        let bound = self.nest.bound_nest_id().await?;
        let now = seams.platform.now_epoch_secs();
        // "Trust this box" is the blessing (`nests.md` § Expiry / renewal →
        // *Duration and blessing*): the standard-length grants below then
        // renew themselves. Recorded on the account plane, under the identity
        // this connection proved, before the grants — so the refresh that ends
        // the tap already renders the box blessed. A blessing the plane
        // refuses (no account runtime yet) does not stop the mint: the grants
        // are the tap's substance, the failure is reported once the refresh
        // has shown them, and the toggle (or a re-tap) records it later.
        let blessing = self.bless(seams, &bound, true, now).await;
        let ledger = seams.ledger.load().await?;

        let mint_holders: Vec<view_model::MintHolder> = holders
            .iter()
            .map(|h| view_model::MintHolder {
                bridge_id: h.bridge_id.clone(),
                role: h.role.clone(),
            })
            .collect();
        let mail = seams.mail.load().await?;
        let period_keys = seams.period_keys.custody().await.unwrap_or_default();
        let options = view_model::mint_options(&period_keys, &mail, &mint_holders);
        // What this box already holds, so a re-run adds nothing. Expired grants
        // do not count as cover — a lapsed capability reads as "background
        // processing paused" (`nests.md` § Expiry / renewal), and re-granting
        // is exactly what the tap is for.
        let held: Vec<_> = grant_log::current_grants(&ledger)
            .into_iter()
            .filter(|g| g.window_end > now)
            .collect();

        // Record-then-deposit: every signed event
        // reaches durable storage BEFORE any blob goes live on the nest.
        // `grant_log::UndepositedGrant` carries the rule and the asymmetry
        // behind it — it is the only way to reach these bytes.
        let mut pending: Vec<grant_log::UndepositedGrant> = Vec::new();
        let mut intents = SuccessionLedger::events_replica(ActorId(seams.actor_id), Vec::new());
        for option in options {
            // Every catalog option carries at least one candidate by
            // construction, and the holder it names is one the roster just
            // reported — both `continue`s are unreachable today and are here so
            // a future catalog change degrades by skipping one option rather
            // than failing the whole tap.
            let Some(bridge_id) = option.holder_candidates.first() else {
                continue;
            };
            let Some(holder) = holders.iter().find(|h| &h.bridge_id == bridge_id) else {
                continue;
            };
            if held.iter().any(|g| {
                g.holder == holder.pubkey && option.scope.iter().all(|s| g.scope.contains(s))
            }) {
                continue;
            }

            let scope_tuples: Vec<ScopeTuple> = option
                .scope
                .iter()
                .map(|s| ScopeTuple {
                    class: s.class.clone(),
                    kind: s.kind.clone(),
                    tier: s.tier.clone(),
                    set: None,
                    factor: None,
                })
                .collect();
            let window_end = now + DEFAULT_GRANT_WINDOW_SECS;
            let grant_id = seams.platform.new_grant_id();
            let blob = mint_grant(
                Some(&period_keys),
                &mail,
                &seams.actor_id,
                &grant_id,
                &holder.pubkey,
                holder.mlkem_ek.as_deref(),
                GrantWindow(now, window_end),
                &scope_tuples,
            )?;
            let blob_bytes = blob.to_canonical_bytes()?;

            let unsigned = grant_log::build_mint_event(
                grant_id,
                holder.pubkey,
                option.scope.clone(),
                now,
                window_end,
                now,
            );
            let signed = seams.signer.sign_grant_event(unsigned)?;
            grant_log::append_signed(&mut intents, signed);
            pending.push(grant_log::UndepositedGrant::new(grant_id, blob_bytes));
        }

        if pending.is_empty() {
            let refreshed = self.refresh().await;
            return blessing.and(refreshed);
        }
        let stored = seams.ledger.merge(intents).await?;
        let recorded = grant_log::RecordedGrants::from_stored(&stored);

        // Now the deposits. A failure here — a refused blob, or a ledger write
        // that did not record one of our events — leaves a PHANTOM row: recorded, visible on
        // the Nests page, and cleared by an idempotent revoke. Never the
        // inverse, which nothing on the wire could discover.
        let mut deposit_failed: Option<PairDispatchError> = None;
        for grant in pending {
            let outcome = match grant.release(&recorded) {
                Ok(blob_bytes) => self.nest.mint_grant(blob_bytes).await.map_err(Into::into),
                Err(e) => Err(e.into()),
            };
            if let Err(e) = outcome {
                deposit_failed = Some(e);
                break;
            }
        }

        // The refresh runs on BOTH paths: a partly-failed tap must still show
        // the user every grant it recorded, not leave them invisible behind an
        // error until some later refresh happens to run.
        let refreshed = self.refresh().await;
        match deposit_failed {
            Some(e) => Err(e),
            None => blessing.and(refreshed),
        }
    }

    /// Renew a grant's window (`fauna.capabilities.renew`, master-key regime →
    /// `appended_keys` empty) + a signed `Renew` event carrying the scope
    /// forward.
    ///
    /// **The log records the extension first, then the nest bumps** — the same
    /// asymmetry [`grant_log::UndepositedGrant`] states for mint, applied to the
    /// only other call that *widens* a capability. Where the two can disagree,
    /// the log must be the MORE permissive of the pair. Nest-first (the shape
    /// before 2026-08-14) skewed the dangerous way on a failed `save_cas`: the
    /// nest honouring 90 more days while the page showed the grant lapsing on
    /// its old end, so the user believed an expiry that had not happened and had
    /// no reason to revoke. Log-first shows a window the holder may not actually
    /// get — visible, and settled by an idempotent revoke. Reordering also stops
    /// an unknown `grant_id` from bumping the nest before
    /// [`grant_log::build_renew_event`] rejects it.
    ///
    /// (Revoke needs no such change: it *narrows*, so its nest-first order
    /// already leaves the log the more permissive of the two.)
    async fn renew(&self, grant_id: Vec<u8>) -> Result<(), PairDispatchError> {
        let seams = self.trust_seams()?;
        let grant_id = grant_id_array(&grant_id)?;
        self.set_status(LinkedNestStatus::Working);

        let now = seams.platform.now_epoch_secs();
        let ledger = seams.ledger.load().await?;
        let current = grant_log::current_grants(&ledger);
        let grant = current
            .iter()
            .find(|g| g.grant_id == grant_id.as_slice())
            .ok_or(grant_log::RenewError::GrantNotFound)?;
        // A renewal keeps the duration the user picked at mint and slides the
        // window (`grant_log::renewal_window`): one mint-length ahead of now,
        // one behind, so a bounded grant's wrap set never grows past the
        // calendar.
        let (new_start, new_end) = grant_log::renewal_window(
            grant,
            grant_log::renewal_window_secs(&ledger, &grant_id),
            now,
        );
        // A bounded mail grant's renewal carries the epoch wraps for the
        // window it extends into, computed BEFORE the log records the
        // extension: a grant whose wraps cannot be sealed (its holder left
        // the roster) is refused with its recorded end unmoved — a keyless
        // bump would move it, and the next keyed renewal would wrap from
        // there and skip the epochs in between. The roster round trip is
        // paid only for a bounded grant; a master-key grant is a pure move.
        let appended_keys = if grant_log::is_bounded_mail_grant(&grant.scope) {
            let holders = self.nest.content_processor_holders().await?;
            renewal_keys_for(
                &seams.mail.load().await?,
                &seams.actor_id,
                &holders,
                grant,
                new_end,
            )?
        } else {
            Vec::new()
        };
        let unsigned = grant_log::build_renew_event(&ledger, &grant_id, new_start, new_end, now)?;
        let signed = seams.signer.sign_grant_event(unsigned)?;
        seams
            .ledger
            .merge(SuccessionLedger::events_replica(
                ActorId(seams.actor_id),
                vec![signed],
            ))
            .await?;

        let bumped = self
            .nest
            .renew_grant(grant_id, new_start, new_end, appended_keys)
            .await;
        let refreshed = self.refresh().await;
        bumped?;
        refreshed
    }

    /// Revoke a grant (`fauna.capabilities.revoke` + a signed `Revoke` event).
    /// The holder for the event is read from the current log; the nest deletion
    /// runs first, then the log records the revoke (which drops the grant from
    /// the Now lens but keeps it in History).
    async fn revoke_grant_action(&self, grant_id: Vec<u8>) -> Result<(), PairDispatchError> {
        let seams = self.trust_seams()?;
        let grant_id = grant_id_array(&grant_id)?;
        self.set_status(LinkedNestStatus::Working);

        let ledger = seams.ledger.load().await?;
        let holder = grant_log::current_grants(&ledger)
            .into_iter()
            .find(|g| g.grant_id == grant_id.as_slice())
            .map(|g| {
                let mut h = [0u8; 32];
                h.copy_from_slice(&g.holder);
                h
            })
            .ok_or_else(|| {
                PairDispatchError::InvalidState("no current grant with that id to revoke".into())
            })?;
        self.nest.revoke_grant(grant_id).await?;

        let unsigned =
            grant_log::build_revoke_event(grant_id, holder, seams.platform.now_epoch_secs());
        let signed = seams.signer.sign_grant_event(unsigned)?;
        seams
            .ledger
            .merge(SuccessionLedger::events_replica(
                ActorId(seams.actor_id),
                vec![signed],
            ))
            .await?;
        self.refresh().await
    }

    /// **Keep** a grant the aftermath carried across (`succession-aftermath.md`
    /// § Adjudicating what the aftermath carries across): clear its review
    /// mark, touching neither the grant nor the ledger.
    ///
    /// Deliberately **no nest round-trip and no grant event** — unlike
    /// [`revoke_grant_action`](Self::revoke_grant_action) this changes nothing
    /// about the capability, only the owner's verdict on it, which lives
    /// entirely in the succession ledger's grant-mark rows. Keep closes *this* raising event, never the row
    /// forever: a later succession re-stamps the mark, because a verdict about
    /// one compromise window cannot vouch across the next one.
    ///
    /// A grant with no mark is a **no-op that still succeeds**: the owner may
    /// have kept it on another device already, and failing here would turn a
    /// converged state into an error.
    async fn keep_grant_action(&self, grant_id: Vec<u8>) -> Result<(), PairDispatchError> {
        let seams = self.trust_seams()?;
        let grant_id = grant_id_array(&grant_id)?;
        self.set_status(LinkedNestStatus::Working);

        fauna_client_config::keep_grant_mark(seams.ledger.as_ref(), &grant_id).await?;
        self.refresh().await
    }

    /// Record the user's blessing verdict for a nest (`nest-trust-blessed-toggle`)
    /// and refresh — which runs the renew sweep, so blessing a nest whose
    /// grants are already due renews them at once.
    async fn set_blessed(&self, nest_id: String, blessed: bool) -> Result<(), PairDispatchError> {
        let seams = self.trust_seams()?;
        let nest_id = parse_nest_id(&nest_id)?;
        self.set_status(LinkedNestStatus::Working);
        self.bless(seams, &nest_id, blessed, seams.platform.now_epoch_secs())
            .await?;
        self.refresh().await
    }

    /// The `AutoRenew` tick: sweep, and refresh only when something renewed
    /// (without sweeping again). Nothing renewed is the common case and costs
    /// the page nothing — no status flicker, no re-list.
    async fn auto_renew(&self) -> Result<(), PairDispatchError> {
        if self.auto_renew_sweep().await == 0 {
            return Ok(());
        }
        self.refresh_with(false).await
    }

    /// **The auto-renew loop's body** (`nests.md` § Expiry / renewal →
    /// *Duration and blessing*): if the connected/home nest is blessed, renew
    /// every grant [`view_model::grants_due_for_renewal`] names — each by its
    /// own mint-time length, all `Renew` events recorded in ONE ledger merge
    /// before any nest bump (log-first, the rule [`Self::renew`] states for
    /// every widening call).
    ///
    /// The *due* decision reads the trust facet's render clock
    /// (`trust_clock`); the renewed windows are stamped from the real one, so a
    /// test that moves the render clock sees a real renewal on the nest.
    ///
    /// **Silent and best-effort**, like [`Self::reconcile_sweep`]: every
    /// failure degrades to renewing less, and a grant the sweep could not renew
    /// keeps approaching its end, where its liveness says so. Returns how many
    /// grants it recorded as renewed (diagnostics only).
    async fn auto_renew_sweep(&self) -> usize {
        let Ok(seams) = self.trust_seams() else {
            return 0;
        };
        // Blessed means the identity this connection proved is blessed —
        // never the nest's own `nest.info` claim, which a box the user never
        // blessed can answer with a blessed sibling's id. A nest whose claim
        // disagrees with its proven identity gets no background widening at
        // all.
        let Ok(SelfNest { id: bound, .. }) = proven_self(&*self.nest).await else {
            return 0;
        };
        if !self.blessing_of(seams, &bound).await {
            return 0;
        }
        let Ok(ledger) = seams.ledger.load().await else {
            return 0;
        };
        let Ok(holders) = self.nest.content_processor_holders().await else {
            return 0;
        };
        // A bounded grant whose wraps cannot be sealed is skipped below; an
        // unreadable mail custody makes every bounded grant such a one.
        let mail = seams.mail.load().await.unwrap_or_default();
        let blessed_holders: BTreeSet<Vec<u8>> =
            holders.iter().map(|h| h.pubkey.to_vec()).collect();
        let real_now = seams.platform.now_epoch_secs();
        let due_now = fauna_client_capabilities::trust_clock::render_now_secs(real_now);
        let current = grant_log::current_grants(&ledger);
        let mut renewals: Vec<GrantRenewal> = Vec::new();
        let mut intents = SuccessionLedger::events_replica(ActorId(seams.actor_id), Vec::new());
        for due in view_model::grants_due_for_renewal(&ledger, &blessed_holders, due_now) {
            let Ok(grant_id) = grant_id_array(&due.grant_id) else {
                continue;
            };
            let Some(grant) = current.iter().find(|g| g.grant_id == due.grant_id) else {
                continue;
            };
            // The slid window (`grant_log::renewal_window`), exactly as the
            // manual renew computes it.
            let (new_start, new_end) =
                grant_log::renewal_window(grant, due.extend_by_secs, real_now);
            // A bounded mail grant's wraps are sealed before its `Renew` is
            // recorded (the rule [`Self::renew`] states): a grant whose wraps
            // cannot be sealed keeps its recorded end and approaches expiry,
            // where its liveness says so.
            let appended_keys = match renewal_keys_for(
                &mail,
                &seams.actor_id,
                &holders,
                grant,
                new_end,
            ) {
                Ok(keys) => keys,
                Err(e) => {
                    tracing::warn!(target: "fauna_pair", "auto-renew: skipping a grant whose epoch keys cannot be sealed: {e}");
                    continue;
                }
            };
            let Ok(unsigned) =
                grant_log::build_renew_event(&ledger, &grant_id, new_start, new_end, real_now)
            else {
                continue;
            };
            let Ok(signed) = seams.signer.sign_grant_event(unsigned) else {
                continue;
            };
            grant_log::append_signed(&mut intents, signed);
            renewals.push((grant_id, new_start, new_end, appended_keys));
        }
        if renewals.is_empty() || seams.ledger.merge(intents).await.is_err() {
            return 0;
        }
        let renewed = renewals.len();
        for (grant_id, new_start, new_end, appended_keys) in renewals {
            if let Err(e) = self
                .nest
                .renew_grant(grant_id, new_start, new_end, appended_keys)
                .await
            {
                tracing::warn!(target: "fauna_pair", "auto-renew: nest bump failed: {e}");
            }
        }
        tracing::info!(target: "fauna_pair", "auto-renew: renewed {renewed} grant(s)");
        renewed
    }

    /// Flip a nest row's trust-facet lens (Now ⇄ History) in place — local UI
    /// state, no nest round-trip. Matches the home row first, then the pairings.
    fn set_lens(&self, nest_id: String, lens: TrustLens) -> Result<(), PairDispatchError> {
        let mut inner = self.inner.lock().expect("snapshot mutex");
        if let Some(home) = inner.snapshot.home.as_mut()
            && home.nest_id == nest_id
        {
            home.lens = lens;
            return Ok(());
        }
        for row in inner.snapshot.pairings.iter_mut() {
            if row.nest_id == nest_id {
                row.lens = lens;
                return Ok(());
            }
        }
        Err(PairDispatchError::InvalidState(format!(
            "no nest row with id {nest_id} to switch lens on"
        )))
    }

    async fn link(
        &self,
        nest_id: String,
        capabilities: Vec<String>,
        expires_at: Option<i64>,
        label: Option<String>,
        nest_url: Option<String>,
    ) -> Result<(), PairDispatchError> {
        let private_nest_id = parse_nest_id(&nest_id)?;
        // Empty → the canonical full self-sync set (design § 3). Per-capability
        // scoping from the UI is a future refinement.
        let capabilities = if capabilities.is_empty() {
            default_self_sync()
        } else {
            capabilities
        };
        self.set_status(LinkedNestStatus::Working);
        self.nest
            .add(PairAddRequest {
                private_nest_id: ByteBuf::from(private_nest_id),
                capabilities,
                expires_at,
                label,
                nest_url,
                extra: Default::default(),
            })
            .await?;
        self.refresh().await
    }

    /// Link two of the user's nests with one action, seeding both ends. Discovers
    /// the connected nest's id + the other nest's id (via `fauna.nest.info`),
    /// then writes the reciprocal `fauna.pair.add` rows: `{private_nest_id:
    /// other_id}` on the connected nest, `{private_nest_id: this_id}` on the
    /// other. Both rows default to the full self-sync capability set
    /// ([`default_self_sync`] — includes the `mail_pull` relay grant) when the UI
    /// passes none. Re-lists the connected nest's pairings on success.
    ///
    /// Discovery + the second connection happen **before** any write, so a
    /// connect/discovery failure leaves both nests untouched. The peer (other
    /// nest) row is written first, then the connected nest's row, so a failure
    /// after the peer write leaves the connected nest's list (what the user sees)
    /// unchanged — the user retries and `fauna.pair.add` re-issue is idempotent.
    async fn link_both(
        &self,
        other_nest_url: String,
        capabilities: Vec<String>,
        expires_at: Option<i64>,
        label: Option<String>,
    ) -> Result<(), PairDispatchError> {
        let capabilities = if capabilities.is_empty() {
            default_self_sync()
        } else {
            capabilities
        };
        self.set_status(LinkedNestStatus::Working);

        // Discover both nests' identities, then open the second connection —
        // before any write. Both ids are the ones the respective connection
        // PROVED, never a nest's own `nest.info` claim: a row names the nest
        // it grants to, so a connected box claiming a sibling's id would have
        // the peer authorize that sibling instead, and a peer claiming one
        // would plant a row here that grants the user's capabilities to a nest
        // they never linked. Either lie refuses the whole action with nothing
        // written.
        let this = proven_self(&*self.nest).await?;
        // The link action delivers first: a successor signing in at a nest
        // that never heard of its succession would be a stranger there, and
        // the retired key would go on signing in. Never a reason to stop.
        self.deliver_succession_first(&other_nest_url).await;
        let peer = self.nest.connect_peer(&other_nest_url).await?;
        let other = proven_self(&*peer).await?;

        // The chain follows the link: before either row, the two nests are
        // brought to one RecoveryKey registration chain, so the nest being
        // linked can verify a succession — and holds the owner's chain before
        // any seed thief can register one of their own there. A fork refuses
        // the link, and so does a reconcile that could not finish: a fork is
        // never linked silently.
        self.reconcile_recovery_chain(&*peer).await?;

        // Reciprocal rows. The other nest authorizes *this* nest …
        peer.add(PairAddRequest {
            private_nest_id: ByteBuf::from(this.id.clone()),
            capabilities: capabilities.clone(),
            expires_at,
            label: None,
            nest_url: Some(this.url.clone()),
            extra: Default::default(),
        })
        .await?;
        // … and this (connected) nest authorizes the other. The user's label
        // describes the nest they linked (the other), so it rides this row — the
        // one shown in the list the user is looking at.
        self.nest
            .add(PairAddRequest {
                private_nest_id: ByteBuf::from(other.id.clone()),
                capabilities,
                expires_at,
                label,
                nest_url: Some(other_nest_url.clone()),
                extra: Default::default(),
            })
            .await?;

        // Re-list so the user sees the new pairing *before* the post-link hook
        // runs — a hook failure then leaves the pairing visible plus an error
        // banner, rather than hiding the (already-durable) link.
        self.refresh().await?;

        // Post-link side effect (e.g. mail auto-provisions the user's mailbox
        // onto the just-linked home box, reusing the fleet MSEK — the one-action
        // home-with-public-relay flow). A no-op when no hook is wired. Status
        // stays `Working` across it so the UI reflects the in-flight provisioning;
        // an error surfaces (via `dispatch`) without undoing the link.
        if let Some(hook) = &self.post_link {
            self.set_status(LinkedNestStatus::Working);
            let result = hook.after_link_both(&other_nest_url).await;
            self.set_status(LinkedNestStatus::Idle);
            result?;
        }
        Ok(())
    }

    /// **The link action delivers first** (`identity-succession.md` §
    /// Enforcement on the home nest → *Every nest the identity is linked to*,
    /// **The road**'s last sentence): when the linking identity has
    /// predecessors, their statement path — read from the connected nest — is
    /// submitted at `other_nest_url` over an anonymous connection, before the
    /// link signs in there. So re-linking lands the succession at a nest no
    /// owed list named.
    ///
    /// Nothing here can stop the link. A nest that cannot verify the statement
    /// (it holds no chain for the retired identity: bound 2) refuses it, and
    /// the link goes on to sign in as the successor exactly as it would have:
    /// a sign-in never registers an account, so a successor the nest does not
    /// know is refused there and nothing is written under either identity.
    /// The refusal is the runtime road's to answer, with the retired seed.
    async fn deliver_succession_first(&self, other_nest_url: &str) {
        let path = match self.nest.predecessor_statements().await {
            Ok(path) if path.is_empty() => return,
            Ok(path) => path,
            Err(e) => {
                tracing::warn!(target: "fauna_pair", error = %e, "link: predecessor statements unread; linking without delivering");
                return;
            }
        };
        let hops = path.len();
        match self.nest.submit_succession_at(other_nest_url, path).await {
            Ok(landed) => {
                tracing::info!(target: "fauna_pair", hops, landed, "link: succession delivered before connecting");
            }
            Err(reason) => {
                tracing::warn!(target: "fauna_pair", hops, ?reason, "link: succession not delivered; linking anyway");
            }
        }
    }

    /// The both-ends link's chain reconcile
    /// (`identity-succession.md` § Enforcement on the home nest → *Every nest
    /// the identity is linked to*, clause (a)): whichever of the connected
    /// nest and `peer` holds the longer registration chain extends the other,
    /// over the account's own two connections. A link the next reconcile
    /// still owes (a seed-alone link, which no submit carries) does not stop
    /// the link; a fork does, and so does a nest that could not be read or
    /// refused a record.
    async fn reconcile_recovery_chain(
        &self,
        peer: &dyn LinkedNestsNest,
    ) -> Result<(), PairDispatchError> {
        use fauna_client_core::recovery_chain::{ChainReconcile, reconcile_registration_chains};
        let bound = SeamChainDoor::new(&*self.nest);
        let linked = SeamChainDoor::new(peer);
        match reconcile_registration_chains(&bound, &linked).await {
            Ok(ChainReconcile::Forked) => Err(PairDispatchError::RecoveryKeysDiffer),
            Ok(outcome) => {
                tracing::info!(target: "fauna_pair", ?outcome, "link: recovery-key chain reconciled");
                Ok(())
            }
            Err(e) => Err(bound.refusal().or_else(|| linked.refusal()).map_or_else(
                || PairDispatchError::InvalidState(e.to_string()),
                PairDispatchError::Nest,
            )),
        }
    }

    async fn unlink(&self, nest_id: String) -> Result<(), PairDispatchError> {
        let private_nest_id = parse_nest_id(&nest_id)?;
        self.set_status(LinkedNestStatus::Working);
        self.sweep_escrow_at(&private_nest_id).await;
        self.nest.revoke(private_nest_id).await?;
        self.refresh().await
    }

    /// **Unlinking ends the replica** (`account-sync-plane.md` § The bind leg,
    /// ruling 4): when the nest being unlinked can be reached, the account's
    /// escrow wraps there are deleted, one generation at a time
    /// (`fauna.generation.escrow.delete`), after which the sealed rows left
    /// there open for no one who lacks a device's keys. Best-effort and logged,
    /// never a reason to keep the link: a nest that cannot be reached keeps
    /// what it was given until the account is deleted there. The connection's
    /// bound identity must be the row's nest id — wraps are never deleted at a
    /// box that merely answers the address.
    async fn sweep_escrow_at(&self, private_nest_id: &[u8]) {
        let url = match self.nest.list().await {
            Ok(reply) => reply
                .pairings
                .into_iter()
                .find(|p| p.private_nest_id.as_slice() == private_nest_id)
                .and_then(|p| p.nest_url),
            Err(e) => {
                tracing::warn!("unlink: the pairing could not be read, no escrow sweep: {e}");
                return;
            }
        };
        let Some(url) = url.filter(|u| !u.trim().is_empty()) else {
            tracing::info!("unlink: the linked nest has no address on record — no escrow sweep");
            return;
        };
        let peer = match self.nest.connect_peer(&url).await {
            Ok(peer) => peer,
            Err(e) => {
                tracing::warn!("unlink: {url} could not be reached, its escrow wraps stay: {e}");
                return;
            }
        };
        match peer.bound_nest_id().await {
            Ok(id) if id == private_nest_id => {}
            Ok(_) => {
                tracing::warn!(
                    "unlink: {url} is bound to another identity than the pairing — no escrow \
                     sweep there"
                );
                return;
            }
            Err(e) => {
                tracing::warn!("unlink: {url}'s identity could not be read, no escrow sweep: {e}");
                return;
            }
        }
        let generations = match peer.escrow_generations().await {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!("unlink: the escrow wraps at {url} could not be listed: {e}");
                return;
            }
        };
        let mut deleted = 0usize;
        for generation in generations {
            match peer.escrow_delete(generation).await {
                Ok(()) => deleted += 1,
                Err(e) => tracing::warn!("unlink: an escrow wrap at {url} stays: {e}"),
            }
        }
        tracing::info!(
            deleted,
            "unlink: the account's escrow wraps at {url} are deleted"
        );
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl LinkedNestsMachine {
    pub fn snapshot(&self) -> LinkedNestsSnapshot {
        fauna_core::clone_locked(&self.inner, |i| &i.snapshot)
    }
}

// The async surface. Every Rust shell (tui, linux, the wasm twin) awaits these
// inline on its own executor; the UniFFI apps reach the same four calls through
// the exported twins `fauna_uniffi_async::export` generates, which run them on a
// tokio runtime worker, off the foreign poll thread.
#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl LinkedNestsMachine {
    /// Initial page load.
    pub async fn hydrate(&self) -> Result<(), PairDispatchError> {
        self.refresh().await
    }

    /// The connected nest's own identity + address as the nest reports them
    /// (`fauna.nest.info` over the authed connection — see
    /// [`LinkedNestsNest::this_nest`]). A thin passthrough so every app reaches
    /// the same WS-RPC node-identity query through one surface (priority
    /// #2/#4). **Display only:** the id is the nest's unverified claim about
    /// itself. Anything that decides on the id — a seed custody, the seed-map
    /// fan-out, a pairing row, a cert's delivery target — reads
    /// [`Self::bound_nest_id`] instead. The seam error folds into
    /// [`PairDispatchError::Nest`] so it shares the FFI-crossing error type.
    pub async fn this_nest(&self) -> Result<SelfNest, PairDispatchError> {
        Ok(self.nest.this_nest().await?)
    }

    /// The identity this connection is **bound** to — the one every trust
    /// decision keyed on "which nest is this" reads (`security.md` § Transport
    /// trust: pin the identity, never the cert): the BR-2 comparand a seed
    /// custody derives to, the entry a rotation supersedes, both ids a
    /// both-ends link writes, and the
    /// `target_nest_id` a DNS cert issuance seals to (`tls-certificates.md`
    /// § C.3 D7). One surface for all 7 apps: linux and tui reach it through
    /// `resolve_this_nest_id`, windows/apple/android through `fauna-ffi`, web
    /// through the wasm machine.
    ///
    /// Reads [`LinkedNestsNest::bound_nest_id`] (the login's pin for this
    /// origin, else a possession proof over this connection) and **refuses
    /// a nest whose own `fauna.nest.info` claim disagrees with it** — never
    /// the claim alone, which any box can answer with a sibling's id it
    /// learned from the pairing list, and never a silent substitution
    /// either: a box that misreports its identity is hostile or broken, and
    /// neither gets a custody write, a fan-out, a pairing row or a cert on
    /// the strength of the connection.
    pub async fn bound_nest_id(&self) -> Result<Vec<u8>, PairDispatchError> {
        Ok(proven_self(&*self.nest).await?.id)
    }

    pub async fn dispatch(&self, action: LinkedNestsAction) -> Result<(), PairDispatchError> {
        // Clear any prior error before the new action runs. The restore outcome
        // goes with it: it describes *the last restore*, so leaving it standing
        // across an unrelated action would let a stale "restored" caption sit
        // over a page the user has since moved on from.
        // The background renew tick is not the user acting, so it leaves the
        // page's standing error and restore caption alone.
        if action != LinkedNestsAction::AutoRenew {
            let mut inner = self.inner.lock().expect("snapshot mutex");
            inner.snapshot.error = None;
            inner.snapshot.restore_outcome = None;
        }
        let result = match action {
            LinkedNestsAction::Refresh => self.refresh().await,
            LinkedNestsAction::Link {
                nest_id,
                capabilities,
                expires_at,
                label,
                nest_url,
            } => {
                self.link(nest_id, capabilities, expires_at, label, nest_url)
                    .await
            }
            LinkedNestsAction::LinkBoth {
                other_nest_url,
                capabilities,
                expires_at,
                label,
            } => {
                self.link_both(other_nest_url, capabilities, expires_at, label)
                    .await
            }
            LinkedNestsAction::Unlink { nest_id } => self.unlink(nest_id).await,
            LinkedNestsAction::RetryForwards => self.retry_forwards().await,
            LinkedNestsAction::DiscardForwards => self.discard_forwards().await,
            LinkedNestsAction::Mint {
                nest_id,
                holder_bridge_id,
                scope,
                duration,
            } => self.mint(nest_id, holder_bridge_id, scope, duration).await,
            LinkedNestsAction::SetBlessed { nest_id, blessed } => {
                self.set_blessed(nest_id, blessed).await
            }
            LinkedNestsAction::AutoRenew => self.auto_renew().await,
            LinkedNestsAction::MintDefaultSet => self.mint_default_set().await,
            LinkedNestsAction::Renew { grant_id } => self.renew(grant_id).await,
            LinkedNestsAction::Revoke { grant_id } => self.revoke_grant_action(grant_id).await,
            LinkedNestsAction::KeepGrant { grant_id } => self.keep_grant_action(grant_id).await,
            LinkedNestsAction::RevokeBackupSeal => self.revoke_backup_seal().await,
            LinkedNestsAction::RevokeBackupWriter { destination_id } => {
                self.revoke_backup_writer(destination_id).await
            }
            LinkedNestsAction::RestoreGeneration {
                destination_id,
                folder_name,
                path_hash,
                manifest_hash,
            } => {
                self.restore_generation(destination_id, folder_name, path_hash, manifest_hash)
                    .await
            }
            LinkedNestsAction::SetLens { nest_id, lens } => self.set_lens(nest_id, lens),
        };
        if let Err(ref e) = result {
            // Producer-side log for the reactive `error-message` banner: fire
            // once here where the state is set, not in the per-tick render
            // (observability.md § Log on the *event*, not the *paint*).
            let message = e.to_string();
            tracing::warn!(target: "fauna_pair", "{message}");
            let mut inner = self.inner.lock().expect("snapshot mutex");
            inner.snapshot.error = Some(message);
            inner.snapshot.status = LinkedNestStatus::Idle;
        }
        result
    }
}

/// Dispatch `MintDefaultSet` fire-and-forget, logging only — the one-tap
/// `trust_prompt` glue every native app shares (`onboarding.md` § 3b-ter),
/// deduped out of tui's and linux's independently-spawned identical bodies.
/// Best-effort and log-only **by design**: the user has completed sign-in,
/// and a mint failure must not paint an error over that — the same trust is
/// grantable any time from Settings → Nests, which is also where the minted
/// grants and their log entries surface. **Which grants** is not decided
/// here: `MintDefaultSet` mints exactly what the shared mint catalog
/// derives, so this glue holds no policy that could drift from the Nests
/// page's own picker.
pub async fn dispatch_mint_default_trust_set(machine: LinkedNestsMachine) {
    match machine.dispatch(LinkedNestsAction::MintDefaultSet).await {
        Ok(()) => {
            let minted = machine
                .snapshot()
                .home
                .map_or(0, |row| row.trust_grants.len());
            tracing::info!(minted, "one-tap trust: default grant set minted");
        }
        Err(e) => tracing::warn!(
            "one-tap trust: minting the default set failed ({e}); \
             the same trust can be granted from Settings → Nests"
        ),
    }
}

// ── helpers ─────────────────────────────────────────────────────────

/// Parse the user-entered nest identity (a 32-byte Ed25519 public key in hex)
/// into raw bytes. An out-of-band short code is a documented future refinement.
fn parse_nest_id(s: &str) -> Result<Vec<u8>, PairDispatchError> {
    decode_hex32(s).ok_or_else(|| {
        PairDispatchError::InvalidState(format!(
            "nest identity must be a 32-byte Ed25519 public key in hex (64 hex chars); got {} chars",
            s.trim().len()
        ))
    })
}

/// Coerce a `Renew`/`Revoke` action's `grant_id` bytes into the fixed 16-byte
/// handle the capability RPCs + grant log use. A wrong length is a client-side
/// precondition failure (the UI passed a malformed handle).
fn grant_id_array(bytes: &[u8]) -> Result<[u8; 16], PairDispatchError> {
    bytes.try_into().map_err(|_| {
        PairDispatchError::InvalidState(format!("grant id must be 16 bytes; got {}", bytes.len()))
    })
}

/// The `appended_keys` a grant's renewal to `new_end` carries, for both renew
/// paths (the manual `Renew` and the auto-renew sweep): empty for a master-key
/// grant (a pure window bump), and for a **bounded** mail grant one canonical
/// `WrappedScopeKey` per sealing epoch the extension newly covers
/// (`bounded_mail_renewal_keys`, from the log's recorded end — the nest
/// refuses a bounded extension that leaves one uncovered). The wraps are
/// sealed to the holder as the LIVE roster names it: the log keeps the
/// holder's pubkey only, never its seal-target key material (the rotation-heal
/// driver's rule), so a holder the roster no longer lists gets no wrap and the
/// caller refuses or skips — never a classical-blind wrap to a stale target.
/// Every wrap carries the grant's own labeler factor, exactly as the mint's
/// did (`nests.md` § Expiry / renewal → *Duration and blessing*).
fn renewal_keys_for(
    mail: &fauna_core::data::MailConfig,
    actor_id: &[u8; 32],
    holders: &[HolderInfo],
    grant: &grant_log::CurrentGrant,
    new_end: u64,
) -> Result<Vec<Vec<u8>>, PairDispatchError> {
    if !grant_log::is_bounded_mail_grant(&grant.scope) {
        return Ok(Vec::new());
    }
    let holder_pk: [u8; 32] = grant.holder.as_slice().try_into().map_err(|_| {
        PairDispatchError::InvalidState("this grant's holder is not a 32-byte pubkey".into())
    })?;
    let Some(holder) = holders.iter().find(|h| h.pubkey == holder_pk) else {
        return Err(PairDispatchError::InvalidState(
            "this grant's holder is no longer on the nest's roster, so its epoch keys cannot \
             be sealed to it — revoke it and mint the trust afresh"
                .into(),
        ));
    };
    let factor = grant_log::labeler_factor_of_grant(&grant.scope);
    let wraps = fauna_client_capabilities::bounded_mail_renewal_keys(
        mail,
        actor_id,
        &holder_pk,
        holder.mlkem_ek.as_deref(),
        grant.window_end,
        new_end,
        factor.as_deref(),
    )?;
    wraps
        .iter()
        .map(|w| w.to_canonical_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Decode a 64-char hex string into 32 raw bytes; `None` when it is not exactly
/// 64 hex chars. Shared by [`parse_nest_id`] (the user-typed identity) and
/// the native seam's `fauna.nest.info` reply decode (the nest's own id).
/// Thin `Option<Vec<u8>>` wrapper over the canonical
/// [`fauna_core::hex32::decode`] — every call site here predates the
/// `[u8; 32]`-returning array form.
fn decode_hex32(s: &str) -> Option<Vec<u8>> {
    fauna_core::hex32::decode(s).ok().map(|arr| arr.to_vec())
}

fn to_hex(bytes: &[u8]) -> String {
    fauna_core::format::hex_full(bytes)
}

// ── WS-RPC client wrapper ───────────────────────────────────────────

/// Typed WS-RPC client for the `fauna.pair.*` kinds, generic over the shared
/// [`RpcRequester`] transport (native `Arc<NestClient>` / wasm `WsRpcClient`) —
/// the pairing analogue of `fauna-client-dns`'s `DnsAdminClient`. The kind /
/// payload composition is written once here, not duplicated per client.
pub struct PairClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> PairClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.pair.list` — owner-implicit list of the connection actor's
    /// pairings. Replay-safe pure read.
    pub async fn list(&self) -> Result<PairListReply, R::Error> {
        self.nest
            .request(
                "fauna.pair.list",
                PairListRequest {
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.pair.add` — authorize a pairing (owner-scoped; the connection
    /// actor scopes the write).
    pub async fn add(&self, req: PairAddRequest) -> Result<PairAddReply, R::Error> {
        self.nest.request("fauna.pair.add", req).await
    }

    /// `fauna.pair.revoke` — revoke a pairing by the linked nest's id.
    pub async fn revoke(&self, private_nest_id: Vec<u8>) -> Result<PairRevokeReply, R::Error> {
        self.nest
            .request(
                "fauna.pair.revoke",
                PairRevokeRequest {
                    private_nest_id: ByteBuf::from(private_nest_id),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.pair.forward_retry` — re-arm the caller's queued forwards
    /// (owner-implicit; idempotent on replay).
    pub async fn forward_retry(&self) -> Result<PairForwardRetryReply, R::Error> {
        self.nest
            .request(
                "fauna.pair.forward_retry",
                PairForwardRetryRequest {
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.pair.forward_discard` — drop the caller's queued forwards
    /// (owner-implicit; idempotent on replay).
    pub async fn forward_discard(&self) -> Result<PairForwardDiscardReply, R::Error> {
        self.nest
            .request(
                "fauna.pair.forward_discard",
                PairForwardDiscardRequest {
                    extra: Default::default(),
                },
            )
            .await
    }
}

/// Map a transport error into the seam's two-class [`PairNestError`] via the
/// shared [`fauna_protocol::nest_seam_error`] classifier — mirrors
/// `fauna-client-dns`'s `nest_error`.
pub fn nest_error<E: RpcErrorClass + core::fmt::Display>(e: E) -> PairNestError {
    fauna_protocol::nest_seam_error(e)
}

/// The nest at the far end of `nest`'s connection, as the connection PROVED
/// it: [`SelfNest`] whose `id` is [`LinkedNestsNest::bound_nest_id`] and whose
/// `url` is the address dialled. The nest's own `fauna.nest.info` claim is read
/// only to be checked — a nest claiming any other id than the one this
/// connection is bound to is refused ([`PairNestError::Rejected`]), since a
/// disagreement is hostile or broken and a silent substitution would hide it.
/// Shared by [`LinkedNestsMachine::bound_nest_id`], the blessing sweep and
/// both ends of a both-ends link (the peer seam is a [`LinkedNestsNest`] too).
async fn proven_self(nest: &dyn LinkedNestsNest) -> Result<SelfNest, PairNestError> {
    let bound = nest.bound_nest_id().await?;
    let claimed = nest.this_nest().await?;
    if claimed.id != bound {
        tracing::warn!(
            target: "fauna_pair",
            "the nest reports an identity other than the one this connection proved; \
             refusing to act on either"
        );
        return Err(PairNestError::Rejected(
            "the nest reports an identity other than the one this connection proved".into(),
        ));
    }
    Ok(SelfNest {
        id: bound,
        url: claimed.url,
    })
}

/// [`LinkedNestsNest::bound_nest_id`]'s one body for both seams: the identity
/// the login pinned for this connection's origin (`pinned`, read by each seam
/// from its own platform's pin store), else — no pin, i.e. a plaintext
/// loopback nest — a possession proof over `conn` itself: the box signs a
/// fresh nonce of ours as the identity it claims. Never `fauna.nest.info`.
async fn bound_identity<R>(conn: &R, pinned: Option<[u8; 32]>) -> Result<Vec<u8>, PairNestError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    use fauna_client_core::nest_trust::{LoginBindingError, read_bound_identity};
    match read_bound_identity(conn, pinned).await {
        Ok(id) => Ok(id.to_vec()),
        Err(LoginBindingError::Refused(e)) | Err(LoginBindingError::Transport(e)) => {
            Err(nest_error(e))
        }
        Err(e) => Err(PairNestError::Rejected(e.to_string())),
    }
}

/// [`LinkedNestsNest::escrow_generations`] over any transport — one
/// unfiltered `fauna.generation.escrow.get`, each generation named once.
async fn escrow_generations_over<R>(conn: &R) -> Result<Vec<Vec<u8>>, PairNestError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    use fauna_protocol::generation_escrow::{EscrowGetReply, EscrowGetRequest, KIND_ESCROW_GET};
    let reply: EscrowGetReply = conn
        .request(KIND_ESCROW_GET, EscrowGetRequest::default())
        .await
        .map_err(nest_error)?;
    let mut generations: Vec<Vec<u8>> = reply
        .wraps
        .into_iter()
        .map(|w| w.generation_id.into_vec())
        .collect();
    generations.sort();
    generations.dedup();
    Ok(generations)
}

/// [`LinkedNestsNest::registration_chain`] over any transport, for the account
/// the connection is authenticated as.
async fn registration_chain_over<R>(
    conn: &R,
    actor_id: &[u8; 32],
) -> Result<Vec<Vec<u8>>, PairNestError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    fauna_client_core::recovery_chain::fetch_registration_chain(conn, actor_id)
        .await
        .map_err(nest_error)
}

/// [`LinkedNestsNest::submit_registration`] over any transport.
async fn submit_registration_over<R>(conn: &R, record: &[u8]) -> Result<(), PairNestError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    fauna_client_core::recovery_chain::submit_registration_record(conn, record)
        .await
        .map_err(nest_error)
}

/// [`LinkedNestsNest::predecessor_statements`] over any transport, for the
/// account the connection is authenticated as.
async fn predecessor_statements_over<R>(conn: &R) -> Result<Vec<Vec<u8>>, PairNestError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    fauna_client_core::succession_delivery::fetch_predecessor_statements(conn)
        .await
        .map_err(nest_error)
}

/// One nest's registration doors for the chain reconcile, over the pairing
/// seam. The reconcile reports a failure as text; the seam's own error is kept
/// here so the link refuses with the class the nest answered in (a dropped
/// connection stays transient, a refusal stays a refusal).
struct SeamChainDoor<'a> {
    nest: &'a dyn LinkedNestsNest,
    refusal: Mutex<Option<PairNestError>>,
}

impl<'a> SeamChainDoor<'a> {
    fn new(nest: &'a dyn LinkedNestsNest) -> Self {
        Self {
            nest,
            refusal: Mutex::new(None),
        }
    }

    /// The seam error behind the reconcile's failure, when this side raised it.
    fn refusal(&self) -> Option<PairNestError> {
        self.refusal.lock().expect("refusal mutex").take()
    }

    fn keep<T>(&self, result: Result<T, PairNestError>) -> Result<T, String> {
        result.map_err(|e| {
            let message = e.to_string();
            *self.refusal.lock().expect("refusal mutex") = Some(e);
            message
        })
    }
}

impl fauna_client_core::recovery_chain::RegistrationChainDoor for SeamChainDoor<'_> {
    async fn registration_chain(&self) -> Result<Vec<Vec<u8>>, String> {
        self.keep(self.nest.registration_chain().await)
    }

    async fn submit_registration(&self, record: &[u8]) -> Result<(), String> {
        self.keep(self.nest.submit_registration(record.to_vec()).await)
    }
}

/// [`LinkedNestsNest::escrow_delete`] over any transport.
async fn escrow_delete_over<R>(conn: &R, generation_id: Vec<u8>) -> Result<(), PairNestError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    use fauna_protocol::generation_escrow::{
        EscrowDeleteReply, EscrowDeleteRequest, KIND_ESCROW_DELETE,
    };
    let _: EscrowDeleteReply = conn
        .request(
            KIND_ESCROW_DELETE,
            EscrowDeleteRequest {
                generation_id: generation_id.into(),
                extra: Default::default(),
            },
        )
        .await
        .map_err(nest_error)?;
    Ok(())
}

// ── trust-facet seam impls (shared across native + wasm) ─────────────
//
// The **signer** + **platform** seams touch no transport — the identity key +
// the system clock/CSPRNG are the same everywhere — so they are one impl each
// here, not per-target twins (priority #2/#3).

/// Real [`GrantEventSigner`]: holds the user's Ed25519 `SigningKey` (cloned from
/// the owner `ActorKeypair`) and signs a fully-populated (placeholder-`sig`)
/// [`GrantEvent`] in place. The raw key never crosses the machine's FFI boundary
/// (`key-material-hierarchy.md` #7); mirrors mail-settings' `RpcIdentitySigner`.
struct RealGrantEventSigner {
    key: ed25519_dalek::SigningKey,
}

impl GrantEventSigner for RealGrantEventSigner {
    fn sign_grant_event(
        &self,
        event: fauna_core::grant_event::GrantEvent,
    ) -> Result<fauna_core::grant_event::GrantEvent, TrustSignerError> {
        event
            .sign(&self.key)
            .map_err(|e| TrustSignerError::Sign(e.to_string()))
    }
}

/// Real [`TrustPlatform`]: the system clock (`Timestamp::now_secs`, cross-target
/// js-Date on wasm / `SystemTime` native) stamps grant windows + event `at`, and
/// a CSPRNG (`rand::thread_rng`, wasm via getrandom's `wasm_js` backend) mints
/// the 16-byte `grant_id`. Transport-free → one impl for native + wasm.
struct SystemTrustPlatform;

impl TrustPlatform for SystemTrustPlatform {
    fn now_epoch_secs(&self) -> u64 {
        // `now_secs` is `i64`; clamp the (impossible pre-1970) negative case to 0
        // so the grant-window/`at` unit stays a non-negative epoch second.
        fauna_core::data::Timestamp::now_secs().max(0) as u64
    }

    fn new_grant_id(&self) -> [u8; 16] {
        use rand::RngCore;
        let mut id = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut id);
        id
    }
}

/// A proved nest id as the fixed-width box id the backup state keys on.
fn bound_array(bound: &[u8]) -> Option<[u8; 32]> {
    bound.try_into().ok()
}

/// The bound box's destination list off the backup-state seam — the list the
/// trust facet's backup rows, the writer revoke and the generation restore
/// resolve destination URLs from (the client's own pinned list, never the
/// source's word).
async fn backup_destinations_of(
    backup: &BackupTrustSeams,
    bound: &[u8],
) -> Result<Vec<fauna_core::data::BackupDestination>, PairDispatchError> {
    let box_id = bound_array(bound).ok_or_else(|| {
        PairDispatchError::InvalidState("the bound nest id is not 32 bytes".into())
    })?;
    Ok(backup.state.backup_state(box_id).await?.backup.destinations)
}

/// Assemble a [`TrustSeams`] from the identity and the host's stores. The
/// signer + platform are the shared impls above. Keeps the `TrustSeams` wiring
/// in one place.
#[allow(clippy::too_many_arguments)]
fn assemble_trust_seams(
    actor_id: [u8; 32],
    signing_key: ed25519_dalek::SigningKey,
    ledger: Arc<dyn SuccessionLedgerStore>,
    blessings: Arc<dyn BlessedNestsStore>,
    period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
    mail: Arc<dyn fauna_client_config::MailStore>,
    backup: Option<BackupTrustSeams>,
) -> TrustSeams {
    TrustSeams {
        actor_id,
        ledger,
        period_keys,
        mail,
        signer: Arc::new(RealGrantEventSigner { key: signing_key }),
        platform: Arc::new(SystemTrustPlatform),
        blessings,
        backup,
        folder_names: None,
    }
}

// The seam over a concrete transport. A single generic `impl<R> LinkedNestsNest`
// is impossible (async_trait `+ Send` futures vs AFIT `RpcRequester` whose
// future is not provably `Send` generically), so the seam binds the concrete
// transport per target: native `Arc<NestClient>` here, wasm `WsRpcClient` below.
#[cfg(not(target_arch = "wasm32"))]
mod native_seam {
    use super::*;
    use fauna_client::NestClient;
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::discovery::{NestInfoReply, NestInfoRequest};

    struct RpcLinkedNestsNest {
        nest: Arc<NestClient>,
    }

    impl RpcLinkedNestsNest {
        /// A typed `fauna.pair.*` client over the held connection. Cheap (wraps
        /// the `Arc`), so built per call rather than stored alongside `nest`.
        fn pair(&self) -> PairClient<Arc<NestClient>> {
            PairClient::new(self.nest.clone())
        }
        /// Typed `fauna.capabilities.*` client over the held connection (trust facet).
        fn caps(&self) -> CapabilitiesClient<Arc<NestClient>> {
            CapabilitiesClient::new(self.nest.clone())
        }
        /// Mail-admin client — holder discovery (`list_service_users` +
        /// `fetch_bridge_pubkey`) for the trust facet.
        fn admin(&self) -> MailAdminClient<Arc<NestClient>> {
            MailAdminClient::new(self.nest.clone())
        }
    }

    #[async_trait]
    impl LinkedNestsNest for RpcLinkedNestsNest {
        async fn list(&self) -> Result<PairListReply, PairNestError> {
            self.pair().list().await.map_err(nest_error)
        }

        async fn add(&self, req: PairAddRequest) -> Result<(), PairNestError> {
            self.pair().add(req).await.map(|_| ()).map_err(nest_error)
        }

        async fn revoke(&self, private_nest_id: Vec<u8>) -> Result<(), PairNestError> {
            self.pair()
                .revoke(private_nest_id)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn forward_retry(&self) -> Result<u64, PairNestError> {
            self.pair()
                .forward_retry()
                .await
                .map(|r| r.rearmed)
                .map_err(nest_error)
        }

        async fn forward_discard(&self) -> Result<u64, PairNestError> {
            self.pair()
                .forward_discard()
                .await
                .map(|r| r.discarded)
                .map_err(nest_error)
        }

        async fn this_nest(&self) -> Result<SelfNest, PairNestError> {
            // `fauna.nest.info` is an anonymous-discovery kind, but it carries no
            // permission-class gate, so it answers on the authenticated
            // connection too (`bins/fauna-nest/src/routes.rs` skips the
            // pre-identity gate for authed conns). The connected nest reports its
            // own Ed25519 id; the url is the address we are connected on.
            let reply: NestInfoReply = self
                .nest
                .request("fauna.nest.info", NestInfoRequest::default())
                .await
                .map_err(nest_error)?;
            let id = decode_hex32(&reply.nest_id).ok_or_else(|| {
                PairNestError::Rejected(format!("nest reported a malformed id: {}", reply.nest_id))
            })?;
            Ok(SelfNest {
                id,
                url: self.nest.nest_url().to_string(),
            })
        }

        async fn bound_nest_id(&self) -> Result<Vec<u8>, PairNestError> {
            // The host's pin was graduated by the login's SPKI compare, and
            // the bearer connection's TLS is pinned to that SPKI — so on
            // `https` it names the box at the far end of THIS connection.
            let host = fauna_anon_client::trust::authority_of(&self.nest.nest_url());
            super::bound_identity(&self.nest, fauna_anon_client::trust::pinned_identity(&host))
                .await
        }

        async fn connect_peer(
            &self,
            peer_url: &str,
        ) -> Result<Arc<dyn LinkedNestsNest>, PairNestError> {
            // The user's identity is registered on both nests, so we open a
            // second authenticated connection to the other nest with the *same*
            // keypair (reconstructed from the connected nest's auth) and seam
            // over it. `request`/`connect` wait for the WS to come up internally
            // (bounded by the kind deadline), so no explicit Connected wait.
            let keypair = ActorKeypair::from_secret(
                *self
                    .nest
                    .auth()
                    .keypair()
                    .expect("identity keypair required for pairing")
                    .secret_bytes(),
            );
            let peer = NestClient::new(peer_url.to_string(), keypair);
            peer.connect().await.map_err(nest_error)?;
            Ok(Arc::new(RpcLinkedNestsNest { nest: peer }))
        }

        async fn escrow_generations(&self) -> Result<Vec<Vec<u8>>, PairNestError> {
            super::escrow_generations_over(&self.nest).await
        }

        async fn escrow_delete(&self, generation_id: Vec<u8>) -> Result<(), PairNestError> {
            super::escrow_delete_over(&self.nest, generation_id).await
        }

        async fn registration_chain(&self) -> Result<Vec<Vec<u8>>, PairNestError> {
            super::registration_chain_over(&self.nest, &self.nest.auth().actor_id()).await
        }

        async fn submit_registration(&self, record: Vec<u8>) -> Result<(), PairNestError> {
            super::submit_registration_over(&self.nest, &record).await
        }

        async fn predecessor_statements(&self) -> Result<Vec<Vec<u8>>, PairNestError> {
            super::predecessor_statements_over(&self.nest).await
        }

        async fn submit_succession_at(
            &self,
            nest_url: &str,
            path: Vec<Vec<u8>>,
        ) -> Result<usize, OwedReason> {
            // Anonymous, because the statement names the account it retires
            // and the successor may hold none there yet. Nothing checks which
            // nest answers: a statement is a public artifact (`lookup` serves
            // it to anyone), so an impostor learns nothing from it, and the
            // link's own sign-in proves the nest right after.
            let anon = fauna_anon_client::AnonymousNestClient::connect(nest_url)
                .await
                .map_err(|e| OwedReason::Unreachable(e.to_string()))?;
            fauna_client_core::succession_delivery::submit_statement_path(&anon, &path).await
        }

        async fn content_processor_holders(&self) -> Result<Vec<HolderInfo>, PairNestError> {
            discover_holders(&self.admin()).await
        }

        async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), PairNestError> {
            self.caps()
                .mint(grant_blob)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn renew_grant(
            &self,
            grant_id: [u8; 16],
            new_epoch_start: u64,
            new_epoch_end: u64,
            appended_keys: Vec<Vec<u8>>,
        ) -> Result<(), PairNestError> {
            self.caps()
                .renew(grant_id, new_epoch_start, new_epoch_end, appended_keys)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), PairNestError> {
            self.caps()
                .revoke(grant_id)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn reconcile_grants(&self) -> Result<Vec<[u8; 16]>, PairNestError> {
            self.caps().reconcile().await.map_err(nest_error)
        }
    }

    /// Build a [`LinkedNestsMachine`] over a native bearer WS-RPC handle for the
    /// user-settings `linked-nests` page — the constructor `fauna-ffi` + linux
    /// call instead of hand-rolling [`LinkedNestsNest`] glue (priority #2/#4).
    pub fn build_linked_nests_machine(nest: Arc<NestClient>) -> LinkedNestsMachine {
        LinkedNestsMachine::new(Arc::new(RpcLinkedNestsNest { nest }))
    }

    /// Like [`build_linked_nests_machine`] but wires a [`PostLinkHook`] (the
    /// generic post-`LinkBoth` side effect). The mail consumer
    /// (`fauna-client-mail-settings::rpc_glue::build_linked_nests_machine_with_mail_relay`)
    /// calls this to attach mailbox auto-provisioning — the pairing crate stays
    /// mail-agnostic; it only accepts the trait object.
    pub fn build_linked_nests_machine_with_hook(
        nest: Arc<NestClient>,
        post_link: Arc<dyn PostLinkHook>,
    ) -> LinkedNestsMachine {
        LinkedNestsMachine::with_post_link(Arc::new(RpcLinkedNestsNest { nest }), post_link)
    }

    /// Like [`build_linked_nests_machine`] but wires the trust-facet seams
    /// (Nests page) so the machine hydrates the home nest's trust facet + drives
    /// Mint/Renew/Revoke. Builds the seams internally from `(nest, keypair)` —
    /// the signer (raw key behind the seam) and the platform clock/CSPRNG —
    /// plus the host's `ledger` (the grant-event log,
    /// `fauna.state.succession-ledger` — the account-store handle, typically a
    /// `fauna_client_config::ResolvingLedgerStore` over it), so the per-app
    /// shell supplies only the identity and its store, exactly like
    /// mail-settings' `build_mail_settings_machine` (priority #2/#4).
    ///
    /// `blessings` is the seat's account-plane blessing door
    /// (`fauna_account_seams::blessed_nests::PlaneBlessedNests`).
    pub fn build_linked_nests_machine_with_trust(
        nest: Arc<NestClient>,
        keypair: ActorKeypair,
        ledger: Arc<dyn SuccessionLedgerStore>,
        backup_state: Arc<dyn BackupStateStore>,
        blessings: Arc<dyn BlessedNestsStore>,
        period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
        mail: Arc<dyn fauna_client_config::MailStore>,
    ) -> LinkedNestsMachine {
        let trust = build_native_trust_seams(
            &nest,
            keypair,
            ledger,
            backup_state,
            blessings,
            period_keys,
            mail,
        );
        LinkedNestsMachine::new_with_trust(Arc::new(RpcLinkedNestsNest { nest }), trust)
    }

    /// The full-fat native builder: the mailbox auto-provision hook *and* the
    /// trust facet, from one machine (the mail consumer's Nests page).
    #[allow(clippy::too_many_arguments)]
    pub fn build_linked_nests_machine_with_hook_and_trust(
        nest: Arc<NestClient>,
        post_link: Arc<dyn PostLinkHook>,
        keypair: ActorKeypair,
        ledger: Arc<dyn SuccessionLedgerStore>,
        backup_state: Arc<dyn BackupStateStore>,
        blessings: Arc<dyn BlessedNestsStore>,
        period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
        mail: Arc<dyn fauna_client_config::MailStore>,
    ) -> LinkedNestsMachine {
        let trust = build_native_trust_seams(
            &nest,
            keypair,
            ledger,
            backup_state,
            blessings,
            period_keys,
            mail,
        );
        LinkedNestsMachine::with_post_link_and_trust(
            Arc::new(RpcLinkedNestsNest { nest }),
            post_link,
            trust,
        )
    }

    /// Build the trust seams over the native transport: the shared signer +
    /// platform over the host's stores. The raw key goes only into the signer
    /// seam, never across the FFI boundary.
    fn build_native_trust_seams(
        nest: &Arc<NestClient>,
        keypair: ActorKeypair,
        ledger: Arc<dyn SuccessionLedgerStore>,
        backup_state: Arc<dyn BackupStateStore>,
        blessings: Arc<dyn BlessedNestsStore>,
        period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
        mail: Arc<dyn fauna_client_config::MailStore>,
    ) -> TrustSeams {
        let actor_id = keypair.actor_id().0;
        let signing_key = keypair.signing_key().clone();
        let backup = Some(BackupTrustSeams {
            state: backup_state,
            source: Arc::new(RpcBackupNest {
                client: fauna_client_backup::BackupClient::new(Arc::clone(nest)),
            }),
            connector: Arc::new(NativeDestinationConnector {
                owner_secret: keypair.signing_key().to_bytes(),
            }),
        });
        super::assemble_trust_seams(
            actor_id,
            signing_key,
            ledger,
            blessings,
            period_keys,
            mail,
            backup,
        )
    }

    // The `fauna.backup.*` seam over the native transport, generated by the
    // shared backup-crate macro — no kind name is re-spelled here.
    fauna_client_backup::impl_backup_nest_seam!(struct RpcBackupNest<Arc<NestClient>>);

    /// Opens the client's **own** authenticated connection to a backup
    /// destination, as the owner. Same shape as the enroll path's
    /// `segment_backup::resolve_destination_connected` (`NestClient::new` +
    /// `connect`), which is what proves a second-origin authed session works;
    /// this one skips the identity re-resolve because the trust row's
    /// authorization check *is* the writer-grant list it then reads.
    struct NativeDestinationConnector {
        /// The owner's Ed25519 secret — a fresh `ActorKeypair` per connect,
        /// because `ActorKeypair` is deliberately not `Clone`. Same shape the
        /// enroll path's `resolve_destination_connected(owner_secret, …)`
        /// already takes.
        owner_secret: [u8; 32],
    }

    /// The client's own destination connector, standalone — for callers that
    /// need one *without* the whole linked-nests trust machine.
    ///
    /// The Backups page's audit loop is the first: it audits the same
    /// destinations the Nests-page trust facet reads, over the same
    /// owner-authenticated second-origin session, but it lives on a different
    /// page and holds no `LinkedNestsMachine`. Exposing the constructor rather
    /// than the struct keeps every native app on one copy of the connect
    /// sequence (priority #2) — windows / apple / android will each want it
    /// through the FFI when their audit shells land.
    pub fn native_backup_destination_connector(
        owner_secret: [u8; 32],
    ) -> Arc<dyn fauna_client_backup::trust::BackupDestinationConnector> {
        Arc::new(NativeDestinationConnector { owner_secret })
    }

    /// The audit's inclusion arm, natively: sampled backup records are fetched
    /// from the **destination's** own public content-addressed blob routes and
    /// opened under the owner's derived `NestBackupKey`.
    ///
    /// `ForeignPublicChunkFetcher` (not `NestPublicChunkFetcher`) is the right
    /// leg precisely because a backup destination is a *foreign* nest to this
    /// client: it is addressed by URL and its blob routes carry no bearer, which
    /// is what lets the audit read it without the destination having to trust
    /// this session for anything beyond the custody list it already answers.
    struct NativeInclusionSource {
        owner_secret: [u8; 32],
        /// The shell's synced-replica reader for covered folders — the mirror
        /// plane's inclusion population. Composed rather than
        /// read here because the replica database and the set-name parser are
        /// the sync engine's, and this crate carries no engine dependency.
        folder_index: Arc<dyn fauna_client_backup::audit::FolderIndexSource>,
    }

    /// Build the native inclusion source. One copy of the fetcher-and-key
    /// construction for every native app (priority #2) — windows / apple /
    /// android reach it through the FFI when their audit shells land, the same
    /// way they will reach [`native_backup_destination_connector`].
    ///
    /// `folder_index` is where this shell reads its synced replica of a covered
    /// folder from — `fauna_sync_engine::segment_backup::ReplicaFolderIndex`
    /// over its sync state dir and bound source nest, or
    /// `fauna_client_backup::audit::NoFolderIndex` for a shell that holds no
    /// replica (the declared absence: the mirror plane then keeps hash-verified
    /// presence over the destination's own list for every covered folder).
    pub fn native_backup_inclusion_source(
        owner_secret: [u8; 32],
        folder_index: Arc<dyn fauna_client_backup::audit::FolderIndexSource>,
    ) -> Arc<dyn fauna_client_backup::audit::BackupInclusionSource> {
        Arc::new(NativeInclusionSource {
            owner_secret,
            folder_index,
        })
    }

    impl fauna_client_backup::audit::BackupInclusionSource for NativeInclusionSource {
        fn fetcher(
            &self,
            destination_nest_url: &str,
        ) -> Arc<dyn fauna_core::file_download::BlobFetcher> {
            Arc::new(fauna_client::ForeignPublicChunkFetcher::new(
                destination_nest_url,
            ))
        }

        fn keys(&self) -> fauna_core::file_download::FileDownloadKeys {
            // The same key the source nest sealed these segments under
            // (`key-material-hierarchy.md` § Path A-sibling-0). Derived per call
            // rather than held: it is one blake3, and the audit runs at most
            // once per destination per `AUDIT_MIN_INTERVAL`.
            fauna_core::file_download::FileDownloadKeys::owner(
                fauna_core::crypto::NestBackupKey::derive(&self.owner_secret),
            )
        }

        fn folder_index(
            &self,
            folder_set: &str,
        ) -> Option<fauna_client_backup::audit::FolderIndex> {
            self.folder_index.folder_index(folder_set)
        }
    }

    #[async_trait]
    impl fauna_client_backup::trust::BackupDestinationConnector for NativeDestinationConnector {
        async fn connect(
            &self,
            url: &str,
        ) -> Result<fauna_client_backup::trust::DestinationConnection, String> {
            // `NestClient::new` already hands back an `Arc`.
            let client = NestClient::new(
                url.to_string(),
                ActorKeypair::from_secret(self.owner_secret),
            );
            client
                .connect()
                .await
                .map_err(|e| format!("connect to backup destination {url}: {e}"))?;
            // The identity this connection is bound to (the host's pin, else a
            // possession proof over it) — what the shared door holds to the
            // enrolled `destination_actor_pubkey`.
            let bound_nest_id = fauna_client::trust::connection_bound_identity(&*client, url)
                .await
                .map_err(|e| format!("backup destination {url} identity: {e}"))?;
            Ok(fauna_client_backup::trust::DestinationConnection {
                seam: Arc::new(RpcBackupNest {
                    client: fauna_client_backup::BackupClient::new(client),
                }),
                bound_nest_id,
            })
        }
    }

    /// Resolve the `nest_actor_id` the just-connected connection is **bound**
    /// to — [`LinkedNestsMachine::bound_nest_id`] over a native bearer handle:
    /// the id the deployment-seed custody leg and rotation drive bind to
    /// (`fauna_client_account_runtime::deployment_seeds`), the source nest a
    /// backup enrollment names, and the `target_nest_id` the tui and linux DNS
    /// cert-issuance glue seals to. Never the nest's own `fauna.nest.info`
    /// claim: a nest whose claim disagrees with the identity the connection
    /// proved is refused (`Err`), so a box claiming a sibling's id gets no
    /// custody write, no rotation mark and no cert.
    /// Lives here (not in `fauna-client-config`) because it is built on
    /// [`build_linked_nests_machine`]'s linked-nests-page machinery —
    /// `fauna-client-config` deliberately does not depend back on this crate.
    pub async fn resolve_this_nest_id(nest: &Arc<NestClient>) -> Result<Vec<u8>, String> {
        build_linked_nests_machine(Arc::clone(nest))
            .bound_nest_id()
            .await
            .map_err(|e| e.to_string())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native_seam::{
    build_linked_nests_machine, build_linked_nests_machine_with_hook,
    build_linked_nests_machine_with_hook_and_trust, build_linked_nests_machine_with_trust,
    native_backup_destination_connector, native_backup_inclusion_source, resolve_this_nest_id,
};

#[cfg(target_arch = "wasm32")]
mod wasm_seam {
    use super::*;
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::RpcRequester;
    use fauna_protocol::discovery::{NestInfoReply, NestInfoRequest};
    use fauna_rpc_wasm::{TokenWsRpcClient, WsRpcClient};

    /// A connector the SPA supplies so the seam can open a *second* authenticated
    /// WS-RPC client to a peer nest for both-ends linking
    /// ([`LinkedNestsAction::LinkBoth`]). Given the peer's `http(s)://` origin it
    /// resolves a connected [`WsRpcClient`] authenticated as the user's *same*
    /// identity — the SPA mints the peer bearer over the CORS-exempt anonymous WS
    /// (`fauna.auth.{challenge,verify}`, the wasm `challengeVerify`), the path the deprecated
    /// cross-origin HTTP `/api/v1/auth/token` fetch could not take. `!Send` (the
    /// browser is single-threaded; `Rc`-based like [`WsRpcClient`]).
    pub type PeerConnect = std::rc::Rc<
        dyn Fn(
            String,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<WsRpcClient, PairNestError>>>,
        >,
    >;

    /// A connector the SPA supplies so the trust facet can open the client's
    /// **own** authenticated WS-RPC client to a *backup destination*, as the
    /// owner — the wasm twin of native's `NativeDestinationConnector`.
    ///
    /// Passed in from `fauna-wasm` for exactly the reason [`PeerConnect`] is:
    /// the two-step anonymous-handshake → [`TokenWsRpcClient`] connect
    /// (`fauna.auth.handshake` over the CORS-exempt anonymous WS, then an
    /// authed session) lives above this crate, and the enroll path already
    /// proves it. It hands back a *connected* client and this module wraps it
    /// in the shared seam, so no client re-spells a `fauna.backup.*` kind.
    ///
    /// This is what keeps the writer-grant row honest on web: the read and the
    /// revoke both travel the destination's own connection, never the source
    /// nest (`nests.md` § Trust facet — backup rows). `!Send`, like everything
    /// on the browser's single thread.
    ///
    /// The closure also hands back the identity the connection **proved** — the
    /// possession proof the handshake it minted the bearer with already read
    /// and checked against the origin's pin (web's structural ceiling: a
    /// browser exposes no received cert) — never the nest's `fauna.nest.info`
    /// claim.
    pub type BackupConnect = std::rc::Rc<
        dyn Fn(
            String,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(TokenWsRpcClient, [u8; 32]), String>>>,
        >,
    >;

    // The `fauna.backup.*` seam over each browser transport, generated by the
    // shared backup-crate macro — the home nest rides the page's existing
    // `WsRpcClient`, a destination its own `TokenWsRpcClient`.
    fauna_client_backup::impl_backup_nest_seam!(struct WasmBackupNest<WsRpcClient>);
    fauna_client_backup::impl_backup_nest_seam!(struct WasmDestinationNest<TokenWsRpcClient>);

    /// Adapts the SPA-supplied [`BackupConnect`] closure to the shared
    /// `BackupDestinationConnector` seam, wrapping the connected client in
    /// [`WasmDestinationNest`] so the row projection stays shared.
    struct WasmDestinationConnector {
        connect: BackupConnect,
    }

    #[async_trait(?Send)]
    impl fauna_client_backup::trust::BackupDestinationConnector for WasmDestinationConnector {
        async fn connect(
            &self,
            url: &str,
        ) -> Result<fauna_client_backup::trust::DestinationConnection, String> {
            let (client, bound_nest_id) = (self.connect)(url.to_string()).await?;
            Ok(fauna_client_backup::trust::DestinationConnection {
                seam: Arc::new(WasmDestinationNest {
                    client: fauna_client_backup::BackupClient::new(client),
                }),
                bound_nest_id,
            })
        }
    }

    /// Build the destination connector on its own — the wasm twin of
    /// [`native_backup_destination_connector`], and for the same reason it
    /// exists: the Backups page's audit loop needs a connector to each
    /// destination *without* the whole linked-nests trust machine (it holds no
    /// `LinkedNestsMachine`). Exposing the constructor rather than
    /// [`WasmDestinationConnector`] keeps web on the one copy of the
    /// connect-and-wrap sequence the trust facet already uses.
    ///
    /// `connect` is the SPA-supplied [`BackupConnect`] — the same closure
    /// `build_linked_nests_machine_with_peer_and_trust` takes, built once in
    /// `fauna-wasm`'s `make_backup_connect`.
    pub fn wasm_backup_destination_connector(
        connect: BackupConnect,
    ) -> Arc<dyn fauna_client_backup::trust::BackupDestinationConnector> {
        Arc::new(WasmDestinationConnector { connect })
    }

    /// The audit's inclusion arm on **web**: sampled backup records are fetched
    /// from the destination's own public content-addressed blob routes and opened
    /// under the owner's derived `NestBackupKey`.
    ///
    /// The wasm twin of native's `NativeInclusionSource`, differing in exactly
    /// one line — the fetcher. `WasmPublicChunkFetcher` is the browser leg of the
    /// same [`fauna_core::file_download::BlobFetcher`] seam
    /// `ForeignPublicChunkFetcher` is natively, and the walk both feed
    /// (`fauna_core::file_download::download_file_bytes_by_manifest`) is not
    /// feature-gated and already runs on wasm — web's per-file snapshot download
    /// calls it. **So the audit needs no declared platform absence on web**, and
    /// in particular does not route through the native-only `SyncEngine` binding
    /// over that walk (`docs/goal/ui/backups.md` § Audit-alert surface, "The
    /// inclusion arm is shared too").
    ///
    /// Both routes carry no bearer by design: the bytes are already sealed, and
    /// integrity rests on the content address each blob is fetched *by*. That is
    /// what lets the audit read the destination without it having to trust this
    /// session for anything beyond the custody list it already answers.
    struct WasmInclusionSource {
        owner_secret: [u8; 32],
    }

    /// Build the web inclusion source. One copy of the fetcher-and-key
    /// construction for the browser, mirroring
    /// [`native_backup_inclusion_source`] (priority #2).
    pub fn wasm_backup_inclusion_source(
        owner_secret: [u8; 32],
    ) -> Arc<dyn fauna_client_backup::audit::BackupInclusionSource> {
        Arc::new(WasmInclusionSource { owner_secret })
    }

    impl fauna_client_backup::audit::BackupInclusionSource for WasmInclusionSource {
        fn fetcher(
            &self,
            destination_nest_url: &str,
        ) -> Arc<dyn fauna_core::file_download::BlobFetcher> {
            // The trailing-slash trim every other `WasmPublicChunkFetcher`
            // construction site does: a destination URL arrives from
            // `BackupState.backup.destinations` as the user typed it, and the
            // fetcher concatenates `/api/v1/...` onto it verbatim.
            Arc::new(fauna_core::file_download::WasmPublicChunkFetcher::new(
                destination_nest_url.trim_end_matches('/'),
            ))
        }

        fn keys(&self) -> fauna_core::file_download::FileDownloadKeys {
            // The same key the source nest sealed these segments under
            // (`key-material-hierarchy.md` § Path A-sibling-0). Derived per call
            // rather than held: it is one blake3, and the audit runs at most
            // once per destination per `AUDIT_MIN_INTERVAL`.
            fauna_core::file_download::FileDownloadKeys::owner(
                fauna_core::crypto::NestBackupKey::derive(&self.owner_secret),
            )
        }

        fn folder_index(
            &self,
            _folder_set: &str,
        ) -> Option<fauna_client_backup::audit::FolderIndex> {
            // **Web's one declared absence on this arm.** The SPA
            // holds no persistent replica of any folder — its file listing is
            // a live `fauna.sync.files` read, the source's word at audit time,
            // which cannot anchor a destination's verdict (a source outage
            // would decide it). So on web the covered-folder mirror plane keeps
            // its floor, hash-verified presence over the destination's own
            // list (`backup-destinations.md` § Ordinary-folder coverage →
            // *Retention + audit*). The reserved rails' ledger anchor is
            // unaffected: it needs no replica.
            None
        }
    }

    struct RpcLinkedNestsNest {
        nest: WsRpcClient,
        /// `Some` once the SPA has wired the both-ends peer connector; `None`
        /// keeps single-end-only behavior (`connect_peer` rejects).
        connect: Option<PeerConnect>,
    }

    impl RpcLinkedNestsNest {
        fn pair(&self) -> PairClient<WsRpcClient> {
            PairClient::new(self.nest.clone())
        }
        /// Typed `fauna.capabilities.*` client over the browser WS (trust facet).
        fn caps(&self) -> CapabilitiesClient<WsRpcClient> {
            CapabilitiesClient::new(self.nest.clone())
        }
        /// Mail-admin client — holder discovery (`list_service_users` +
        /// `fetch_bridge_pubkey`) for the trust facet.
        fn admin(&self) -> MailAdminClient<WsRpcClient> {
            MailAdminClient::new(self.nest.clone())
        }
    }

    #[async_trait(?Send)]
    impl LinkedNestsNest for RpcLinkedNestsNest {
        async fn list(&self) -> Result<PairListReply, PairNestError> {
            self.pair().list().await.map_err(nest_error)
        }

        async fn add(&self, req: PairAddRequest) -> Result<(), PairNestError> {
            self.pair().add(req).await.map(|_| ()).map_err(nest_error)
        }

        async fn revoke(&self, private_nest_id: Vec<u8>) -> Result<(), PairNestError> {
            self.pair()
                .revoke(private_nest_id)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn forward_retry(&self) -> Result<u64, PairNestError> {
            self.pair()
                .forward_retry()
                .await
                .map(|r| r.rearmed)
                .map_err(nest_error)
        }

        async fn forward_discard(&self) -> Result<u64, PairNestError> {
            self.pair()
                .forward_discard()
                .await
                .map(|r| r.discarded)
                .map_err(nest_error)
        }

        async fn this_nest(&self) -> Result<SelfNest, PairNestError> {
            let reply: NestInfoReply = self
                .nest
                .request("fauna.nest.info", NestInfoRequest::default())
                .await
                .map_err(nest_error)?;
            let id = decode_hex32(&reply.nest_id).ok_or_else(|| {
                PairNestError::Rejected(format!("nest reported a malformed id: {}", reply.nest_id))
            })?;
            // The browser SPA's WS-RPC client is bound to one origin; that origin
            // is the connected nest's address.
            Ok(SelfNest {
                id,
                url: self.nest.nest_url().to_string(),
            })
        }

        async fn bound_nest_id(&self) -> Result<Vec<u8>, PairNestError> {
            // The origin's pin, possession-verified at every login and keyed
            // by the same base URL the SPA mints its bearer against. A browser
            // exposes no received cert, so this is web's structural ceiling
            // (`security.md` § Transport trust, the web rows).
            use fauna_client_core::nest_trust::{LocalStoragePinStore, NestIdentityPinStore};
            let pinned = LocalStoragePinStore.get(&self.nest.nest_url());
            super::bound_identity(&self.nest, pinned).await
        }

        async fn connect_peer(
            &self,
            peer_url: &str,
        ) -> Result<Arc<dyn LinkedNestsNest>, PairNestError> {
            // Open a second authenticated WS-RPC client to the peer nest via the
            // SPA-supplied connector (the user's *same* identity, registered on
            // both; bearer minted over the CORS-exempt anonymous WS). The returned
            // client's first request waits for its socket to come up (the wasm
            // `WsRpcClient::request` poll-waits for the reconnect loop, bounded by
            // the kind deadline), so no explicit connect-wait here — mirroring the
            // native seam. Carry the connector forward so the new seam can itself
            // chain (matching native's recursion).
            let connect = self.connect.as_ref().ok_or_else(|| {
                PairNestError::Rejected(
                    "linking a second nest in one action is not yet supported on web".into(),
                )
            })?;
            let peer = connect(peer_url.to_string()).await?;
            Ok(Arc::new(RpcLinkedNestsNest {
                nest: peer,
                connect: self.connect.clone(),
            }))
        }

        async fn escrow_generations(&self) -> Result<Vec<Vec<u8>>, PairNestError> {
            super::escrow_generations_over(&self.nest).await
        }

        async fn escrow_delete(&self, generation_id: Vec<u8>) -> Result<(), PairNestError> {
            super::escrow_delete_over(&self.nest, generation_id).await
        }

        async fn registration_chain(&self) -> Result<Vec<Vec<u8>>, PairNestError> {
            let actor_id = fauna_core::hex32::decode(self.nest.actor_id_hex())
                .map_err(|_| PairNestError::Rejected("this connection names no account".into()))?;
            super::registration_chain_over(&self.nest, &actor_id).await
        }

        async fn submit_registration(&self, record: Vec<u8>) -> Result<(), PairNestError> {
            super::submit_registration_over(&self.nest, &record).await
        }

        async fn predecessor_statements(&self) -> Result<Vec<Vec<u8>>, PairNestError> {
            super::predecessor_statements_over(&self.nest).await
        }

        async fn submit_succession_at(
            &self,
            nest_url: &str,
            path: Vec<Vec<u8>>,
        ) -> Result<usize, OwedReason> {
            // Anonymous, because the statement names the account it retires
            // and the successor may hold none there yet. Nothing checks which
            // nest answers: a statement is a public artifact (`lookup` serves
            // it to anyone), so an impostor learns nothing from it, and the
            // link's own sign-in proves the nest right after.
            // The browser twin of native's dial: the anonymous WS is the
            // CORS-exempt one the peer bearer is minted over.
            let anon = fauna_rpc_wasm::AnonymousWsRpcClient::connect(nest_url)
                .map_err(|e| OwedReason::Unreachable(e.to_string()))?;
            fauna_client_core::succession_delivery::submit_statement_path(&anon, &path).await
        }

        async fn content_processor_holders(&self) -> Result<Vec<HolderInfo>, PairNestError> {
            discover_holders(&self.admin()).await
        }

        async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), PairNestError> {
            self.caps()
                .mint(grant_blob)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn renew_grant(
            &self,
            grant_id: [u8; 16],
            new_epoch_start: u64,
            new_epoch_end: u64,
            appended_keys: Vec<Vec<u8>>,
        ) -> Result<(), PairNestError> {
            self.caps()
                .renew(grant_id, new_epoch_start, new_epoch_end, appended_keys)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), PairNestError> {
            self.caps()
                .revoke(grant_id)
                .await
                .map(|_| ())
                .map_err(nest_error)
        }

        async fn reconcile_grants(&self) -> Result<Vec<[u8; 16]>, PairNestError> {
            self.caps().reconcile().await.map_err(nest_error)
        }
    }

    /// Build a [`LinkedNestsMachine`] over the browser WS-RPC handle for the web
    /// `linked-nests` page — the wasm twin of native's `build_linked_nests_machine`.
    /// Single-end only (`connect_peer` rejects); use
    /// [`build_linked_nests_machine_with_peer`] for the both-ends path.
    pub fn build_linked_nests_machine(nest: WsRpcClient) -> LinkedNestsMachine {
        LinkedNestsMachine::new(Arc::new(RpcLinkedNestsNest {
            nest,
            connect: None,
        }))
    }

    /// Build a [`LinkedNestsMachine`] wired for both-ends linking: `connect` opens
    /// the second authenticated client to a peer nest (`LinkBoth`). The web
    /// `linked-nests` page uses this — the SPA passes a connector that mints the
    /// peer bearer over the anonymous WS — so a nest **address** links both ends
    /// in one action, matching native; the single-end `Link` path is unchanged.
    pub fn build_linked_nests_machine_with_peer(
        nest: WsRpcClient,
        connect: PeerConnect,
    ) -> LinkedNestsMachine {
        LinkedNestsMachine::new(Arc::new(RpcLinkedNestsNest {
            nest,
            connect: Some(connect),
        }))
    }

    /// Like [`build_linked_nests_machine`] but wires the trust-facet seams
    /// (Nests page), building all three from `(nest, keypair)` — the wasm twin of
    /// native's `build_linked_nests_machine_with_trust`. Single-end (`connect:
    /// None`); the both-ends-*and*-trust page uses
    /// [`build_linked_nests_machine_with_peer_and_trust`].
    pub fn build_linked_nests_machine_with_trust(
        nest: WsRpcClient,
        keypair: ActorKeypair,
        ledger: Arc<dyn SuccessionLedgerStore>,
        backup_state: Arc<dyn BackupStateStore>,
        blessings: Arc<dyn BlessedNestsStore>,
        mail: Arc<dyn fauna_client_config::MailStore>,
        backup_connect: Option<BackupConnect>,
        period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
    ) -> LinkedNestsMachine {
        let trust = build_wasm_trust_seams(
            &nest,
            keypair,
            ledger,
            backup_state,
            blessings,
            mail,
            backup_connect,
            period_keys,
        );
        LinkedNestsMachine::new_with_trust(
            Arc::new(RpcLinkedNestsNest {
                nest,
                connect: None,
            }),
            trust,
        )
    }

    /// The full web Nests-page builder: both-ends linking (`connect`) **and** the
    /// trust facet, from one machine — so the web page links a nest by address
    /// *and* shows/mints its trust grants, matching native's
    /// `build_linked_nests_machine_with_hook_and_trust` (priority #1: web is not
    /// a reduced surface).
    pub fn build_linked_nests_machine_with_peer_and_trust(
        nest: WsRpcClient,
        connect: PeerConnect,
        keypair: ActorKeypair,
        ledger: Arc<dyn SuccessionLedgerStore>,
        backup_state: Arc<dyn BackupStateStore>,
        blessings: Arc<dyn BlessedNestsStore>,
        mail: Arc<dyn fauna_client_config::MailStore>,
        backup_connect: Option<BackupConnect>,
        period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
    ) -> LinkedNestsMachine {
        let trust = build_wasm_trust_seams(
            &nest,
            keypair,
            ledger,
            backup_state,
            blessings,
            mail,
            backup_connect,
            period_keys,
        );
        LinkedNestsMachine::new_with_trust(
            Arc::new(RpcLinkedNestsNest {
                nest,
                connect: Some(connect),
            }),
            trust,
        )
    }

    /// Build the trust seams over the browser transport: the shared signer +
    /// platform over the host's stores. Wasm twin of native's
    /// `build_native_trust_seams`.
    fn build_wasm_trust_seams(
        nest: &WsRpcClient,
        keypair: ActorKeypair,
        ledger: Arc<dyn SuccessionLedgerStore>,
        backup_state: Arc<dyn BackupStateStore>,
        blessings: Arc<dyn BlessedNestsStore>,
        mail: Arc<dyn fauna_client_config::MailStore>,
        backup_connect: Option<BackupConnect>,
        period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
    ) -> TrustSeams {
        let actor_id = keypair.actor_id().0;
        let signing_key = keypair.signing_key().clone();
        // The backup seams need BOTH ends: the home nest (seal grant, over the
        // page's own connection) and a per-destination connector (writer
        // grants, over each destination's own). Without the connector there is
        // no honest writer row to render, so the pair is all-or-nothing —
        // `None` leaves the facet showing content-processing grants only.
        let backup = backup_connect.map(|connect| BackupTrustSeams {
            state: backup_state,
            source: Arc::new(WasmBackupNest {
                client: fauna_client_backup::BackupClient::new(nest.clone()),
            }),
            connector: Arc::new(WasmDestinationConnector { connect }),
        });
        super::assemble_trust_seams(
            actor_id,
            signing_key,
            ledger,
            blessings,
            period_keys,
            mail,
            backup,
        )
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm_seam::{
    BackupConnect, PeerConnect, build_linked_nests_machine, build_linked_nests_machine_with_peer,
    build_linked_nests_machine_with_peer_and_trust, build_linked_nests_machine_with_trust,
    wasm_backup_destination_connector, wasm_backup_inclusion_source,
};

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{RecordingRequester, block_on};
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct FakeState {
        pairings: Vec<PairingRow>,
        added: Vec<PairAddRequest>,
        revoked: Vec<Vec<u8>>,
        reject: bool,
        // Trust facet: the nest's content-processor roster + captured capability
        // RPC calls, so tests assert the machine deposited/renewed/revoked.
        holders: Vec<HolderInfo>,
        // When set, `content_processor_holders` fails — models the Admin-gated
        // `list_service_users` denying a non-admin owner (the trust facet is
        // admin-scoped in v1); the home row must still render (empty facet).
        holders_error: bool,
        minted_blobs: Vec<Vec<u8>>,
        /// `(grant_id, new_epoch_end, appended_keys)` per `renew_grant` call —
        /// the keys as the canonical `WrappedScopeKey` bytes the machine sent.
        renewed: Vec<GrantRenewal>,
        revoked_grants: Vec<[u8; 16]>,
        /// When set, `mint_grant` accepts this many deposits and refuses every
        /// one after — an ordinary network blip partway through a batch, which
        /// is all it takes to reach the failure mode.
        mint_accepts_before_refusing: Option<usize>,
        /// `renew_grant` / `revoke_grant` refuse — the other half of the
        /// ordering pins: which side of a split-brain each call leaves behind.
        renew_refuses: bool,
        revoke_grant_refuses: bool,
        /// The `(owner, grant_id)` rows this nest reports from
        /// `fauna.capabilities.reconcile` — deliberately independent of
        /// `minted_blobs`, because the whole point of the sweep is rows the
        /// client's log does NOT account for (an interrupted deposit, a
        /// resurrection, or a hostile invention).
        nest_grant_rows: Vec<[u8; 16]>,
        /// `reconcile_grants` refuses (any refusal or transport fault). The
        /// sweep must degrade to a no-op, never fail the page.
        reconcile_refuses: bool,
        /// Shared ordered call trace (see [`CallTrace`]); `None` in every test
        /// that does not pin ordering.
        trace: Option<CallTrace>,
        /// What `fauna.pair.list` reports as the caller's forward queue
        /// (zeros by default).
        forward_queue: ForwardQueue,
        /// How many times the two forward-queue actions reached the nest.
        forward_retries: usize,
        forward_discards: usize,
        /// The thread each `list` call ran on — where the machine's work was
        /// polled, which the FFI-export stack test pins.
        list_threads: Vec<std::thread::ThreadId>,
        /// The generations this nest holds escrow wraps for (the unlink
        /// sweep's list), and the ones `escrow_delete` removed.
        escrow: Vec<Vec<u8>>,
        escrow_deleted: Vec<Vec<u8>>,
        /// The account's RecoveryKey registration chain at this nest, as
        /// verbatim records; a submit appends (this fake verifies nothing —
        /// the faithful one is `fauna-client-recovery`'s).
        chain: Vec<Vec<u8>>,
        /// `registration_chain` fails as a dropped connection does.
        chain_unreadable: bool,
        /// The statement path `predecessor_statements` serves (the linking
        /// identity's predecessors), and whether that read fails.
        predecessor_statements: Vec<Vec<u8>>,
        predecessors_unreadable: bool,
        /// Every `submit_succession_at` this seam made, as (address, path),
        /// and the refusal it answers with instead of landing them.
        succession_submits: Vec<(String, Vec<Vec<u8>>)>,
        succession_refusal: Option<OwedReason>,
        /// The link's outward steps in order — `"deliver"` and `"connect"`.
        link_steps: Vec<&'static str>,
    }

    struct FakeNest {
        state: StdMutex<FakeState>,
        /// This nest's own id + url, returned by `this_nest`. `self_id` is
        /// also the identity the connection proved (`bound_nest_id`).
        self_id: Vec<u8>,
        self_url: String,
        /// When set, the id `this_nest` (the nest's own `fauna.nest.info`
        /// answer) reports instead of `self_id` — a nest lying about itself.
        /// `bound_nest_id` keeps answering `self_id`.
        claimed_id: StdMutex<Option<Vec<u8>>>,
        /// The seam `connect_peer` hands back (the "other nest"). `None` → a
        /// connect failure.
        peer: StdMutex<Option<Arc<FakeNest>>>,
    }

    impl Default for FakeNest {
        fn default() -> Self {
            Self::with_id(0x01, "https://this.test")
        }
    }

    impl FakeNest {
        fn with_id(id_byte: u8, url: &str) -> Self {
            Self {
                state: StdMutex::new(FakeState::default()),
                self_id: vec![id_byte; 32],
                self_url: url.to_string(),
                claimed_id: StdMutex::new(None),
                peer: StdMutex::new(None),
            }
        }
    }

    #[async_trait]
    impl LinkedNestsNest for FakeNest {
        async fn list(&self) -> Result<PairListReply, PairNestError> {
            let mut s = self.state.lock().unwrap();
            s.list_threads.push(std::thread::current().id());
            if s.reject {
                return Err(PairNestError::Rejected("pairing disabled".into()));
            }
            Ok(PairListReply {
                pairings: s.pairings.clone(),
                forward_queue: s.forward_queue.clone(),
                extra: Default::default(),
            })
        }

        async fn forward_retry(&self) -> Result<u64, PairNestError> {
            let mut s = self.state.lock().unwrap();
            s.forward_retries += 1;
            let rearmed = s.forward_queue.queued;
            Ok(rearmed)
        }

        async fn forward_discard(&self) -> Result<u64, PairNestError> {
            let mut s = self.state.lock().unwrap();
            s.forward_discards += 1;
            let discarded = s.forward_queue.queued;
            // The nest's queue is empty afterwards — what the re-list reads.
            let q = &mut s.forward_queue;
            q.queued = 0;
            q.stuck = 0;
            q.last_error = None;
            Ok(discarded)
        }

        async fn add(&self, req: PairAddRequest) -> Result<(), PairNestError> {
            let mut s = self.state.lock().unwrap();
            if s.reject {
                return Err(PairNestError::Rejected("pairing disabled".into()));
            }
            s.pairings.push(PairingRow {
                extra: Default::default(),
                private_nest_id: req.private_nest_id.clone(),
                capabilities: req.capabilities.clone(),
                expires_at: req.expires_at,
                created_at: 1_700_000_000,
                label: req.label.clone(),
                nest_url: req.nest_url.clone(),
            });
            s.added.push(req);
            Ok(())
        }

        async fn revoke(&self, private_nest_id: Vec<u8>) -> Result<(), PairNestError> {
            let mut s = self.state.lock().unwrap();
            s.pairings
                .retain(|p| p.private_nest_id.as_ref() != private_nest_id.as_slice());
            s.revoked.push(private_nest_id);
            Ok(())
        }

        async fn this_nest(&self) -> Result<SelfNest, PairNestError> {
            let claimed = self.claimed_id.lock().unwrap().clone();
            Ok(SelfNest {
                id: claimed.unwrap_or_else(|| self.self_id.clone()),
                url: self.self_url.clone(),
            })
        }

        async fn bound_nest_id(&self) -> Result<Vec<u8>, PairNestError> {
            Ok(self.self_id.clone())
        }

        async fn connect_peer(
            &self,
            _peer_url: &str,
        ) -> Result<Arc<dyn LinkedNestsNest>, PairNestError> {
            self.state.lock().unwrap().link_steps.push("connect");
            match self.peer.lock().unwrap().clone() {
                Some(p) => Ok(p as Arc<dyn LinkedNestsNest>),
                None => Err(PairNestError::Transient("no peer configured".into())),
            }
        }

        async fn escrow_generations(&self) -> Result<Vec<Vec<u8>>, PairNestError> {
            Ok(self.state.lock().unwrap().escrow.clone())
        }

        async fn escrow_delete(&self, generation_id: Vec<u8>) -> Result<(), PairNestError> {
            let mut s = self.state.lock().unwrap();
            s.escrow.retain(|g| *g != generation_id);
            s.escrow_deleted.push(generation_id);
            Ok(())
        }

        async fn registration_chain(&self) -> Result<Vec<Vec<u8>>, PairNestError> {
            let s = self.state.lock().unwrap();
            if s.chain_unreadable {
                return Err(PairNestError::Transient("chain read dropped".into()));
            }
            Ok(s.chain.clone())
        }

        async fn submit_registration(&self, record: Vec<u8>) -> Result<(), PairNestError> {
            self.state.lock().unwrap().chain.push(record);
            Ok(())
        }

        async fn predecessor_statements(&self) -> Result<Vec<Vec<u8>>, PairNestError> {
            let s = self.state.lock().unwrap();
            if s.predecessors_unreadable {
                return Err(PairNestError::Transient("status read dropped".into()));
            }
            Ok(s.predecessor_statements.clone())
        }

        async fn submit_succession_at(
            &self,
            nest_url: &str,
            path: Vec<Vec<u8>>,
        ) -> Result<usize, OwedReason> {
            let mut s = self.state.lock().unwrap();
            s.link_steps.push("deliver");
            let hops = path.len();
            s.succession_submits.push((nest_url.to_string(), path));
            match s.succession_refusal.clone() {
                Some(reason) => Err(reason),
                None => Ok(hops),
            }
        }

        async fn content_processor_holders(&self) -> Result<Vec<HolderInfo>, PairNestError> {
            let s = self.state.lock().unwrap();
            if s.holders_error {
                return Err(PairNestError::Rejected(
                    "list_service_users is Admin-gated".into(),
                ));
            }
            Ok(s.holders.clone())
        }

        async fn mint_grant(&self, grant_blob: Vec<u8>) -> Result<(), PairNestError> {
            let mut s = self.state.lock().unwrap();
            if let Some(limit) = s.mint_accepts_before_refusing
                && s.minted_blobs.len() >= limit
            {
                return Err(PairNestError::Transient("deposit refused".into()));
            }
            s.minted_blobs.push(grant_blob);
            Ok(())
        }

        async fn renew_grant(
            &self,
            grant_id: [u8; 16],
            new_epoch_start: u64,
            new_epoch_end: u64,
            appended_keys: Vec<Vec<u8>>,
        ) -> Result<(), PairNestError> {
            let mut s = self.state.lock().unwrap();
            if s.renew_refuses {
                return Err(PairNestError::Transient("renew refused".into()));
            }
            s.renewed
                .push((grant_id, new_epoch_start, new_epoch_end, appended_keys));
            Ok(())
        }

        async fn revoke_grant(&self, grant_id: [u8; 16]) -> Result<(), PairNestError> {
            let mut s = self.state.lock().unwrap();
            if s.revoke_grant_refuses {
                return Err(PairNestError::Transient("revoke refused".into()));
            }
            s.revoked_grants.push(grant_id);
            Ok(())
        }

        async fn reconcile_grants(&self) -> Result<Vec<[u8; 16]>, PairNestError> {
            let s = self.state.lock().unwrap();
            if let Some(t) = &s.trace {
                t.lock().unwrap().push("reconcile");
            }
            if s.reconcile_refuses {
                return Err(PairNestError::Rejected("unknown kind".into()));
            }
            Ok(s.nest_grant_rows.clone())
        }
    }

    fn row(id_byte: u8) -> PairingRow {
        PairingRow {
            extra: Default::default(),
            private_nest_id: ByteBuf::from(vec![id_byte; 32]),
            capabilities: default_self_sync(),
            expires_at: None,
            created_at: 1,
            label: None,
            nest_url: None,
        }
    }

    // ── forward queue (`ui/nests.md` § Forward queue) ────────────────

    fn queue(queued: u64, stuck: u64, last_error: Option<&str>) -> ForwardQueue {
        ForwardQueue {
            queued,
            stuck,
            last_error: last_error.map(str::to_string),
            extra: Default::default(),
        }
    }

    /// The FFI exports run the machine on a tokio runtime worker, never on the
    /// thread that polls the exported future. A UniFFI async export is polled
    /// on the foreign executor's thread (Swift's 544 KB cooperative pool), and
    /// `async_runtime = "tokio"` only enters a runtime context there — the
    /// test reproduces exactly that: `Runtime::block_on` polls the future on
    /// this thread (not a worker) with the runtime's context entered. The
    /// page's Revoke writer
    /// action overflowed that stack in a debug build (`refresh` → trust rows
    /// → a destination connect → the WS handshake), so every export spawns
    /// (`docs/goal/architecture/apps/native-async-execution.md` § The rule).
    #[cfg(feature = "uniffi")]
    #[test]
    fn ffi_exports_run_the_machine_off_the_foreign_poll_thread() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let fake = Arc::new(FakeNest::default());
        let m = Arc::new(LinkedNestsMachine::new(fake.clone()));
        rt.block_on(m.clone().__uniffi_async_hydrate()).unwrap();
        rt.block_on(
            m.clone()
                .__uniffi_async_dispatch(LinkedNestsAction::Refresh),
        )
        .unwrap();
        let poll_thread = std::thread::current().id();
        let threads = fake.state.lock().unwrap().list_threads.clone();
        assert_eq!(threads.len(), 2, "hydrate + Refresh each list once");
        assert!(
            threads.iter().all(|t| *t != poll_thread),
            "the machine ran on the foreign poll thread — an FFI export must \
             run through `fauna_uniffi_async::export`"
        );
    }

    /// The list reply's queue lands on the snapshot as the page-level status,
    /// verbatim — counts and the nest's own reason.
    #[tokio::test]
    async fn refresh_projects_the_forward_queue_onto_the_snapshot() {
        let fake = Arc::new(FakeNest::default());
        fake.state.lock().unwrap().forward_queue =
            queue(3, 1, Some("nest not paired for this actor"));
        let m = LinkedNestsMachine::new(fake.clone());
        m.hydrate().await.unwrap();
        assert_eq!(
            m.snapshot().forward_queue,
            Some(ForwardQueueStatus {
                queued: 3,
                stuck: 1,
                last_error: Some("nest not paired for this actor".into()),
            })
        );
    }

    /// The reason is partly the relay's own error code — relay-chosen text
    /// (`private-mode.md` § Post Forwarding: the app renders it as untrusted
    /// text). The projection control-strips it once, here, so no shell's
    /// terminal, label or DOM ever receives an escape or a forged line break;
    /// markup-looking text survives verbatim, inert, for the shell to paint
    /// as plain text.
    #[test]
    fn the_projection_strips_control_characters_from_the_relay_chosen_reason() {
        let status = ForwardQueueStatus::from(queue(1, 0, Some("\u{1b}[31m<b>x</b>\nforbidden")));
        assert_eq!(status.last_error.as_deref(), Some("[31m<b>x</b>forbidden"));
    }

    /// A reason that is nothing but control characters (or empty) projects to
    /// `None`: the page shows no reason line rather than an empty one.
    #[test]
    fn an_empty_or_control_only_reason_projects_to_none() {
        assert_eq!(
            ForwardQueueStatus::from(queue(1, 0, Some(""))).last_error,
            None
        );
        assert_eq!(
            ForwardQueueStatus::from(queue(1, 0, Some("\u{1b}\n"))).last_error,
            None
        );
    }

    /// Before the first refresh the snapshot has no queue (`None`); once the
    /// nest has answered, an empty queue is a checked zero.
    #[tokio::test]
    async fn an_empty_queue_reads_as_a_checked_zero_after_refresh() {
        let fake = Arc::new(FakeNest::default());
        let m = LinkedNestsMachine::new(fake.clone());
        assert_eq!(m.snapshot().forward_queue, None);
        m.hydrate().await.unwrap();
        assert_eq!(
            m.snapshot().forward_queue,
            Some(ForwardQueueStatus {
                queued: 0,
                stuck: 0,
                last_error: None,
            })
        );
    }

    /// `RetryForwards` reaches the nest exactly once and re-lists: the queue
    /// is whatever the nest reports afterwards (the worker sends on its own
    /// pass — the action only makes entries due).
    #[tokio::test]
    async fn retry_forwards_calls_the_nest_then_refreshes() {
        let fake = Arc::new(FakeNest::default());
        fake.state.lock().unwrap().forward_queue = queue(2, 0, Some("refused"));
        let m = LinkedNestsMachine::new(fake.clone());
        m.hydrate().await.unwrap();
        m.dispatch(LinkedNestsAction::RetryForwards).await.unwrap();
        assert_eq!(fake.state.lock().unwrap().forward_retries, 1);
        assert_eq!(fake.state.lock().unwrap().forward_discards, 0);
        let snap = m.snapshot();
        assert_eq!(snap.forward_queue.as_ref().map(|q| q.queued), Some(2));
        assert_eq!(snap.status, LinkedNestStatus::Idle);
        assert_eq!(snap.error, None);
    }

    /// `DiscardForwards` reaches the nest exactly once and re-lists, so the
    /// page shows the queue empty only once the nest confirms it.
    #[tokio::test]
    async fn discard_forwards_calls_the_nest_then_refreshes_to_empty() {
        let fake = Arc::new(FakeNest::default());
        fake.state.lock().unwrap().forward_queue = queue(2, 2, Some("refused"));
        let m = LinkedNestsMachine::new(fake.clone());
        m.hydrate().await.unwrap();
        m.dispatch(LinkedNestsAction::DiscardForwards)
            .await
            .unwrap();
        assert_eq!(fake.state.lock().unwrap().forward_discards, 1);
        assert_eq!(
            m.snapshot().forward_queue,
            Some(ForwardQueueStatus {
                queued: 0,
                stuck: 0,
                last_error: None,
            })
        );
    }

    /// A refused action lands on `error` like every other dispatch — never a
    /// silent no-op (e2e convention 11).
    #[tokio::test]
    async fn a_refused_forward_action_surfaces_on_error() {
        let fake = Arc::new(FakeNest::default());
        let m = LinkedNestsMachine::new(fake.clone());
        m.hydrate().await.unwrap();
        fake.state.lock().unwrap().reject = true;
        // `reject` refuses the re-list after the action; the error must show.
        let err = m
            .dispatch(LinkedNestsAction::RetryForwards)
            .await
            .unwrap_err();
        assert!(matches!(err, PairDispatchError::Nest(_)), "{err:?}");
        assert!(m.snapshot().error.is_some());
        assert_eq!(m.snapshot().status, LinkedNestStatus::Idle);
    }

    #[tokio::test]
    async fn hydrate_lists_pairings_with_hex_id() {
        let fake = Arc::new(FakeNest::default());
        fake.state.lock().unwrap().pairings.push(row(0xab));
        let m = LinkedNestsMachine::new(fake);
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.pairings.len(), 1);
        assert_eq!(snap.pairings[0].nest_id, "ab".repeat(32));
        assert_eq!(snap.pairings[0].capabilities, default_self_sync());
        assert_eq!(snap.status, LinkedNestStatus::Idle);
    }

    #[tokio::test]
    async fn link_adds_parsed_id_with_default_caps_then_refreshes() {
        let fake = Arc::new(FakeNest::default());
        let m = LinkedNestsMachine::new(fake.clone());
        m.dispatch(LinkedNestsAction::Link {
            nest_id: "cd".repeat(32),
            capabilities: vec![], // → default_self_sync()
            expires_at: None,
            label: Some("home NAS".into()),
            nest_url: None,
        })
        .await
        .unwrap();
        {
            let s = fake.state.lock().unwrap();
            assert_eq!(s.added.len(), 1);
            assert_eq!(
                s.added[0].private_nest_id.as_ref(),
                vec![0xcd; 32].as_slice()
            );
            assert_eq!(s.added[0].capabilities, default_self_sync());
        }
        // The add reloads the list, so the snapshot shows the new pairing —
        // including the user-supplied label projected through for the UI.
        assert_eq!(m.snapshot().pairings.len(), 1);
        assert_eq!(m.snapshot().pairings[0].label.as_deref(), Some("home NAS"));
        assert_eq!(m.snapshot().status, LinkedNestStatus::Idle);
    }

    #[tokio::test]
    async fn link_respects_explicit_capabilities() {
        let fake = Arc::new(FakeNest::default());
        let m = LinkedNestsMachine::new(fake.clone());
        m.dispatch(LinkedNestsAction::Link {
            nest_id: "ee".repeat(32),
            capabilities: vec!["mls_pull".into()],
            expires_at: Some(1_900_000_000),
            label: None,
            nest_url: None,
        })
        .await
        .unwrap();
        let s = fake.state.lock().unwrap();
        assert_eq!(s.added[0].capabilities, vec!["mls_pull".to_string()]);
        assert_eq!(s.added[0].expires_at, Some(1_900_000_000));
    }

    /// The capabilities line's display form (linked-nests.md § The surface):
    /// one label per capability, in order — `account_replica` in user voice
    /// through the shared i18n source, the other five as their wire names.
    #[test]
    fn a_pairing_rows_labels_name_the_account_replica_in_user_voice() {
        let row = LinkedNestRow::from(PairingRow {
            private_nest_id: fauna_protocol::ByteBuf::from(vec![7; 32]),
            capabilities: default_self_sync(),
            expires_at: None,
            created_at: 1,
            label: None,
            nest_url: None,
            extra: Default::default(),
        });
        assert_eq!(row.capabilities, default_self_sync(), "the wire names stay");
        let shown: Vec<String> = row
            .capability_labels
            .iter()
            .map(|l| l.clone().resolve(fauna_i18n::strings::lookup))
            .collect();
        assert_eq!(shown.len(), row.capabilities.len());
        assert_eq!(
            shown.last().map(String::as_str),
            Some(fauna_i18n::strings::nests::CAPABILITY_ACCOUNT_REPLICA),
            "the key resolves in the shared catalog"
        );
        assert!(
            !shown.iter().any(|l| l == "account_replica"),
            "the wire name never reaches the line: {shown:?}"
        );
        assert_eq!(
            shown[0], "mls_pull",
            "an unlabelled capability shows its wire name"
        );
    }

    #[tokio::test]
    async fn unlink_revokes_parsed_id_then_refreshes() {
        let fake = Arc::new(FakeNest::default());
        fake.state.lock().unwrap().pairings.push(row(0x11));
        let m = LinkedNestsMachine::new(fake.clone());
        m.hydrate().await.unwrap();
        m.dispatch(LinkedNestsAction::Unlink {
            nest_id: "11".repeat(32),
        })
        .await
        .unwrap();
        assert_eq!(fake.state.lock().unwrap().revoked, vec![vec![0x11u8; 32]]);
        assert_eq!(m.snapshot().pairings.len(), 0);
    }

    /// An unlink over a connected nest whose pairing to `peer` names `row_id`
    /// at the peer's address, the peer holding wraps of two generations.
    async fn unlink_with_peer(row_id: u8, peer: Arc<FakeNest>) -> Arc<FakeNest> {
        let connected = Arc::new(FakeNest::with_id(0xaa, "https://this.test"));
        connected.state.lock().unwrap().pairings.push(PairingRow {
            nest_url: Some("https://peer.test".into()),
            ..row(row_id)
        });
        peer.state.lock().unwrap().escrow = vec![vec![0x01; 32], vec![0x02; 32]];
        *connected.peer.lock().unwrap() = Some(peer);
        let m = LinkedNestsMachine::new(connected.clone());
        m.hydrate().await.unwrap();
        m.dispatch(LinkedNestsAction::Unlink {
            nest_id: format!("{row_id:02x}").repeat(32),
        })
        .await
        .unwrap();
        connected
    }

    /// **Unlinking ends the replica** (`account-sync-plane.md` § The bind leg,
    /// ruling 4): the account's escrow wraps at the unlinked nest are deleted,
    /// every generation of them, and the pairing is revoked.
    #[tokio::test]
    async fn unlink_deletes_the_escrow_wraps_at_the_unlinked_nest() {
        let peer = Arc::new(FakeNest::with_id(0xbb, "https://peer.test"));
        let connected = unlink_with_peer(0xbb, peer.clone()).await;
        let s = peer.state.lock().unwrap();
        assert!(s.escrow.is_empty(), "no wrap is left at the unlinked nest");
        assert_eq!(s.escrow_deleted, vec![vec![0x01; 32], vec![0x02; 32]]);
        assert_eq!(
            connected.state.lock().unwrap().revoked,
            vec![vec![0xbbu8; 32]]
        );
    }

    /// A box answering the address under another identity is never swept: the
    /// wraps stay, and the unlink still revokes.
    #[tokio::test]
    async fn unlink_never_sweeps_a_box_bound_to_another_identity() {
        let impostor = Arc::new(FakeNest::with_id(0xcc, "https://peer.test"));
        let connected = unlink_with_peer(0xbb, impostor.clone()).await;
        let s = impostor.state.lock().unwrap();
        assert!(
            s.escrow_deleted.is_empty(),
            "nothing deleted at an impostor"
        );
        assert_eq!(s.escrow.len(), 2);
        assert_eq!(
            connected.state.lock().unwrap().revoked,
            vec![vec![0xbbu8; 32]]
        );
    }

    /// An unreachable nest keeps its wraps; the link still ends.
    #[tokio::test]
    async fn unlink_of_an_unreachable_nest_still_revokes() {
        let fake = Arc::new(FakeNest::default());
        fake.state.lock().unwrap().pairings.push(PairingRow {
            nest_url: Some("https://gone.test".into()),
            ..row(0x11)
        });
        let m = LinkedNestsMachine::new(fake.clone());
        m.hydrate().await.unwrap();
        m.dispatch(LinkedNestsAction::Unlink {
            nest_id: "11".repeat(32),
        })
        .await
        .unwrap();
        assert_eq!(fake.state.lock().unwrap().revoked, vec![vec![0x11u8; 32]]);
    }

    #[tokio::test]
    async fn link_invalid_nest_id_surfaces_invalid_state_without_calling_nest() {
        let fake = Arc::new(FakeNest::default());
        let m = LinkedNestsMachine::new(fake.clone());
        let err = m
            .dispatch(LinkedNestsAction::Link {
                nest_id: "not-hex".into(),
                capabilities: vec![],
                expires_at: None,
                label: None,
                nest_url: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, PairDispatchError::InvalidState(_)));
        assert!(m.snapshot().error.is_some());
        assert_eq!(fake.state.lock().unwrap().added.len(), 0);
    }

    #[tokio::test]
    async fn nest_rejection_surfaces_in_snapshot() {
        let fake = Arc::new(FakeNest::default());
        fake.state.lock().unwrap().reject = true;
        let m = LinkedNestsMachine::new(fake);
        let result = m.dispatch(LinkedNestsAction::Refresh).await;
        assert!(matches!(
            result,
            Err(PairDispatchError::Nest(PairNestError::Rejected(_)))
        ));
        let snap = m.snapshot();
        assert!(snap.error.is_some());
        assert_eq!(snap.status, LinkedNestStatus::Idle);
    }

    // ── Both-ends linking (LinkBoth) ────────────────────────────────────────

    #[tokio::test]
    async fn link_both_seeds_reciprocal_rows_on_both_nests() {
        // Connected nest A (id 0xaa) with a configured peer nest B (id 0xbb).
        let peer = Arc::new(FakeNest::with_id(0xbb, "https://peer.test"));
        let connected = Arc::new(FakeNest::with_id(0xaa, "https://this.test"));
        *connected.peer.lock().unwrap() = Some(peer.clone());

        let m = LinkedNestsMachine::new(connected.clone());
        m.dispatch(LinkedNestsAction::LinkBoth {
            other_nest_url: "https://peer.test".into(),
            capabilities: vec![], // → default_self_sync()
            expires_at: None,
            label: Some("home NAS".into()),
        })
        .await
        .unwrap();

        // The connected nest got a row naming the OTHER nest (B), carrying the
        // user's label + the other nest's url.
        {
            let a = connected.state.lock().unwrap();
            assert_eq!(a.added.len(), 1, "connected nest gets exactly one row");
            assert_eq!(
                a.added[0].private_nest_id.as_ref(),
                vec![0xbb; 32].as_slice()
            );
            assert_eq!(a.added[0].capabilities, default_self_sync());
            assert_eq!(a.added[0].label.as_deref(), Some("home NAS"));
            assert_eq!(a.added[0].nest_url.as_deref(), Some("https://peer.test"));
        }
        // The other nest got the reciprocal row naming the CONNECTED nest (A),
        // with the connected nest's url and no user label.
        {
            let b = peer.state.lock().unwrap();
            assert_eq!(b.added.len(), 1, "peer nest gets exactly one row");
            assert_eq!(
                b.added[0].private_nest_id.as_ref(),
                vec![0xaa; 32].as_slice()
            );
            assert_eq!(b.added[0].capabilities, default_self_sync());
            assert_eq!(b.added[0].nest_url.as_deref(), Some("https://this.test"));
            assert!(b.added[0].label.is_none());
        }
        // The snapshot re-lists the connected nest's pairings (the A→B row).
        assert_eq!(m.snapshot().pairings.len(), 1);
        assert_eq!(m.snapshot().status, LinkedNestStatus::Idle);
    }

    #[tokio::test]
    async fn link_both_respects_explicit_capabilities_and_expiry() {
        let peer = Arc::new(FakeNest::with_id(0xbb, "https://peer.test"));
        let connected = Arc::new(FakeNest::with_id(0xaa, "https://this.test"));
        *connected.peer.lock().unwrap() = Some(peer.clone());
        let m = LinkedNestsMachine::new(connected.clone());
        m.dispatch(LinkedNestsAction::LinkBoth {
            other_nest_url: "https://peer.test".into(),
            capabilities: vec!["mls_pull".into()],
            expires_at: Some(1_900_000_000),
            label: None,
        })
        .await
        .unwrap();
        // Both rows carry the explicit caps + expiry.
        assert_eq!(
            connected.state.lock().unwrap().added[0].capabilities,
            vec!["mls_pull".to_string()]
        );
        assert_eq!(
            peer.state.lock().unwrap().added[0].expires_at,
            Some(1_900_000_000)
        );
    }

    #[tokio::test]
    async fn link_both_connect_failure_writes_nothing_and_surfaces_error() {
        // No peer configured → `connect_peer` errors (the web "unsupported" /
        // peer-unreachable case). Discovery + connect happen before any write,
        // so the connected nest is left untouched.
        let connected = Arc::new(FakeNest::with_id(0xaa, "https://this.test"));
        let m = LinkedNestsMachine::new(connected.clone());
        let err = m
            .dispatch(LinkedNestsAction::LinkBoth {
                other_nest_url: "https://peer.test".into(),
                capabilities: vec![],
                expires_at: None,
                label: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, PairDispatchError::Nest(_)));
        assert_eq!(
            connected.state.lock().unwrap().added.len(),
            0,
            "a failed both-ends link must not write the connected nest's row"
        );
        let snap = m.snapshot();
        assert!(snap.error.is_some());
        assert_eq!(snap.status, LinkedNestStatus::Idle);
    }

    /// The one id every trust decision reads is the identity this connection
    /// proved. A nest answering `fauna.nest.info` with another id — a
    /// sibling's, learned from the pairing list — is refused outright rather
    /// than believed OR silently corrected, so the seed custody, the fan-out,
    /// the rotation mark and the cert target it feeds all stay put.
    #[tokio::test]
    async fn bound_nest_id_is_the_proven_identity_and_refuses_a_lying_nest() {
        let nest = Arc::new(FakeNest::with_id(0xaa, "https://this.test"));
        let m = LinkedNestsMachine::new(nest.clone());
        assert_eq!(m.bound_nest_id().await.unwrap(), vec![0xaa; 32]);
        // The lying nest still answers `this_nest` with its claim (display),
        // but the trust id is refused.
        *nest.claimed_id.lock().unwrap() = Some(vec![0x02u8; 32]);
        assert_eq!(m.this_nest().await.unwrap().id, vec![0x02u8; 32]);
        let err = m.bound_nest_id().await.unwrap_err();
        assert!(
            matches!(err, PairDispatchError::Nest(PairNestError::Rejected(_))),
            "a claim that disagrees with the proven identity is a refusal, not a substitution: {err:?}"
        );
    }

    /// A both-ends link writes the ids the two connections proved. The
    /// connected nest claiming a sibling's id must not make the peer
    /// authorize that sibling — nothing is written on either nest.
    #[tokio::test]
    async fn link_both_refuses_a_connected_nest_claiming_another_id() {
        let peer = Arc::new(FakeNest::with_id(0xbb, "https://peer.test"));
        let connected = Arc::new(FakeNest::with_id(0xaa, "https://this.test"));
        *connected.peer.lock().unwrap() = Some(peer.clone());
        *connected.claimed_id.lock().unwrap() = Some(vec![0xcc; 32]);
        let m = LinkedNestsMachine::new(connected.clone());
        let err = m
            .dispatch(LinkedNestsAction::LinkBoth {
                other_nest_url: "https://peer.test".into(),
                capabilities: vec![],
                expires_at: None,
                label: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            PairDispatchError::Nest(PairNestError::Rejected(_))
        ));
        assert!(
            peer.state.lock().unwrap().added.is_empty(),
            "no row on the peer"
        );
        assert!(
            connected.state.lock().unwrap().added.is_empty(),
            "no row on the connected nest"
        );
        assert_eq!(m.snapshot().status, LinkedNestStatus::Idle);
    }

    /// The peer side is checked the same way: a peer claiming another id
    /// would plant a row here granting the user's capabilities to a nest
    /// they never linked. Refused before either write.
    #[tokio::test]
    async fn link_both_refuses_a_peer_claiming_another_id() {
        let peer = Arc::new(FakeNest::with_id(0xbb, "https://peer.test"));
        *peer.claimed_id.lock().unwrap() = Some(vec![0xcc; 32]);
        let connected = Arc::new(FakeNest::with_id(0xaa, "https://this.test"));
        *connected.peer.lock().unwrap() = Some(peer.clone());
        let m = LinkedNestsMachine::new(connected.clone());
        let err = m
            .dispatch(LinkedNestsAction::LinkBoth {
                other_nest_url: "https://peer.test".into(),
                capabilities: vec![],
                expires_at: None,
                label: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            PairDispatchError::Nest(PairNestError::Rejected(_))
        ));
        assert!(
            peer.state.lock().unwrap().added.is_empty(),
            "no row on the peer"
        );
        assert!(
            connected.state.lock().unwrap().added.is_empty(),
            "no row on the connected nest"
        );
    }

    // ── The chain follows the link (LinkBoth reconciles before it writes) ────

    /// A first registration and a replacement under its authority, as the
    /// verbatim records a nest serves.
    fn two_link_chain() -> Vec<Vec<u8>> {
        use fauna_core::recovery::{RecoveryKey, RecoveryKeyRegistration};
        let identity = fauna_core::identity::ActorKeypair::from_secret([0x11; 32]);
        let (k1, k2) = (
            RecoveryKey::from_bytes([0x21; 32]),
            RecoveryKey::from_bytes([0x22; 32]),
        );
        let record = |key: &RecoveryKey, prior: Option<&RecoveryKey>, seq: u64| {
            let signed = RecoveryKeyRegistration {
                actor_id: identity.actor_id(),
                recovery_pubkey: key.public(),
                seq,
                created_at: fauna_core::data::Timestamp(1_700_000_000 + seq),
            }
            .sign(identity.signing_key(), key, prior)
            .unwrap();
            fauna_core::encoding::canonical_encode(&signed).unwrap()
        };
        vec![record(&k1, None, 1), record(&k2, Some(&k1), 2)]
    }

    fn link_pair() -> (LinkedNestsMachine, Arc<FakeNest>, Arc<FakeNest>) {
        let peer = Arc::new(FakeNest::with_id(0xbb, "https://peer.test"));
        let connected = Arc::new(FakeNest::with_id(0xaa, "https://this.test"));
        *connected.peer.lock().unwrap() = Some(peer.clone());
        (LinkedNestsMachine::new(connected.clone()), connected, peer)
    }

    fn link_both_action() -> LinkedNestsAction {
        LinkedNestsAction::LinkBoth {
            other_nest_url: "https://peer.test".into(),
            capabilities: vec![],
            expires_at: None,
            label: None,
        }
    }

    /// Clause (a): the link carries the chain to the nest that lacks it,
    /// oldest first, and only then writes the rows — whichever side is behind.
    #[tokio::test]
    async fn link_both_carries_the_recovery_chain_to_the_nest_that_lacks_it() {
        let chain = two_link_chain();
        let (m, connected, peer) = link_pair();
        connected.state.lock().unwrap().chain = chain.clone();
        m.dispatch(link_both_action()).await.unwrap();
        assert_eq!(peer.state.lock().unwrap().chain, chain);
        assert_eq!(peer.state.lock().unwrap().added.len(), 1);
        assert_eq!(connected.state.lock().unwrap().added.len(), 1);

        // The other way round: the nest being linked holds the longer chain.
        let (m, connected, peer) = link_pair();
        connected.state.lock().unwrap().chain = chain[..1].to_vec();
        peer.state.lock().unwrap().chain = chain.clone();
        m.dispatch(link_both_action()).await.unwrap();
        assert_eq!(connected.state.lock().unwrap().chain, chain);
    }

    /// Two chains of which neither extends the other refuse the link with the
    /// page's own wording, and nothing is written at either nest.
    #[tokio::test]
    async fn link_both_refuses_two_nests_holding_different_recovery_keys() {
        let chain = two_link_chain();
        let (m, connected, peer) = link_pair();
        connected.state.lock().unwrap().chain = vec![chain[0].clone()];
        // A different first registration: the replacement record stands in for
        // one — the comparison is over bytes.
        peer.state.lock().unwrap().chain = vec![chain[1].clone()];

        let err = m.dispatch(link_both_action()).await.unwrap_err();
        assert!(matches!(err, PairDispatchError::RecoveryKeysDiffer));
        assert_eq!(
            m.snapshot().error.as_deref(),
            Some(fauna_i18n::strings::nests::LINK_RECOVERY_KEYS_DIFFER)
        );
        for nest in [&connected, &peer] {
            let s = nest.state.lock().unwrap();
            assert!(s.added.is_empty(), "no pairing row");
            assert_eq!(s.chain.len(), 1, "no record submitted");
        }
        assert_eq!(m.snapshot().status, LinkedNestStatus::Idle);
    }

    /// A reconcile that could not finish refuses the link too — a fork is
    /// never linked silently — and keeps the class the nest failed in.
    #[tokio::test]
    async fn link_both_writes_nothing_when_a_chain_cannot_be_read() {
        let (m, connected, peer) = link_pair();
        peer.state.lock().unwrap().chain_unreadable = true;
        let err = m.dispatch(link_both_action()).await.unwrap_err();
        assert!(
            matches!(err, PairDispatchError::Nest(PairNestError::Transient(_))),
            "{err:?}"
        );
        assert!(connected.state.lock().unwrap().added.is_empty());
        assert!(peer.state.lock().unwrap().added.is_empty());
    }

    // ── The link action delivers first (LinkBoth submits a succession) ──────

    /// An identity with predecessors: the link submits their statement path,
    /// read from the connected nest, at the other nest's address BEFORE it
    /// signs in there — and only then links as before.
    #[tokio::test]
    async fn link_both_delivers_the_predecessors_statements_before_it_connects() {
        let path = vec![vec![0x01, 0x02], vec![0x03, 0x04]];
        let (m, connected, peer) = link_pair();
        connected.state.lock().unwrap().predecessor_statements = path.clone();
        m.dispatch(link_both_action()).await.unwrap();

        let s = connected.state.lock().unwrap();
        assert_eq!(
            s.succession_submits,
            vec![("https://peer.test".to_string(), path)],
            "the whole path, oldest hop first, at the address being linked"
        );
        assert_eq!(s.link_steps, vec!["deliver", "connect"], "delivered first");
        assert_eq!(s.added.len(), 1);
        assert_eq!(peer.state.lock().unwrap().added.len(), 1);
    }

    /// An identity that succeeded nobody delivers nothing: no anonymous
    /// connection is opened at all.
    #[tokio::test]
    async fn link_both_delivers_nothing_for_an_identity_without_predecessors() {
        let (m, connected, _peer) = link_pair();
        m.dispatch(link_both_action()).await.unwrap();
        let s = connected.state.lock().unwrap();
        assert!(s.succession_submits.is_empty());
        assert_eq!(s.link_steps, vec!["connect"]);
    }

    /// A refused delivery — the other nest holds no chain for the retired
    /// identity (bound 2), or is unreachable anonymously — and an unreadable
    /// status both leave the link exactly as it would have been: it signs in
    /// and writes both rows.
    #[tokio::test]
    async fn a_refused_or_unread_delivery_does_not_stop_the_link() {
        for reason in [
            OwedReason::Refused("fauna.recovery.not_registered".into()),
            OwedReason::SuccessorExists,
            OwedReason::Unreachable("dial failed".into()),
        ] {
            let (m, connected, peer) = link_pair();
            {
                let mut s = connected.state.lock().unwrap();
                s.predecessor_statements = vec![vec![0x01]];
                s.succession_refusal = Some(reason.clone());
            }
            m.dispatch(link_both_action())
                .await
                .unwrap_or_else(|e| panic!("{reason:?} stopped the link: {e:?}"));
            assert_eq!(
                connected.state.lock().unwrap().link_steps,
                vec!["deliver", "connect"]
            );
            assert_eq!(peer.state.lock().unwrap().added.len(), 1, "{reason:?}");
            assert!(m.snapshot().error.is_none(), "{reason:?}");
        }

        let (m, connected, peer) = link_pair();
        connected.state.lock().unwrap().predecessors_unreadable = true;
        m.dispatch(link_both_action()).await.unwrap();
        assert!(
            connected
                .state
                .lock()
                .unwrap()
                .succession_submits
                .is_empty()
        );
        assert_eq!(peer.state.lock().unwrap().added.len(), 1);
    }

    // ── Post-link hook (auto-provision after LinkBoth) ──────────────────────

    /// Records every `after_link_both` peer URL; can be set to fail to exercise
    /// the surface-but-don't-undo path.
    #[derive(Default)]
    struct FakePostLinkHook {
        calls: StdMutex<Vec<String>>,
        fail: bool,
    }

    #[async_trait]
    impl PostLinkHook for FakePostLinkHook {
        async fn after_link_both(&self, peer_url: &str) -> Result<(), PairDispatchError> {
            self.calls.lock().unwrap().push(peer_url.to_string());
            if self.fail {
                Err(PairDispatchError::Nest(PairNestError::Transient(
                    "provision failed".into(),
                )))
            } else {
                Ok(())
            }
        }
    }

    fn link_both_machine_with_hook(
        hook: Arc<FakePostLinkHook>,
    ) -> (LinkedNestsMachine, Arc<FakeNest>, Arc<FakeNest>) {
        let peer = Arc::new(FakeNest::with_id(0xbb, "https://peer.test"));
        let connected = Arc::new(FakeNest::with_id(0xaa, "https://this.test"));
        *connected.peer.lock().unwrap() = Some(peer.clone());
        let m = LinkedNestsMachine::with_post_link(connected.clone(), hook);
        (m, connected, peer)
    }

    #[tokio::test]
    async fn link_both_fires_post_link_hook_with_peer_url_after_rows_written() {
        let hook = Arc::new(FakePostLinkHook::default());
        let (m, connected, peer) = link_both_machine_with_hook(hook.clone());
        m.dispatch(LinkedNestsAction::LinkBoth {
            other_nest_url: "https://peer.test".into(),
            capabilities: vec![],
            expires_at: None,
            label: Some("home box".into()),
        })
        .await
        .unwrap();

        // Both reciprocal rows are still written …
        assert_eq!(connected.state.lock().unwrap().added.len(), 1);
        assert_eq!(peer.state.lock().unwrap().added.len(), 1);
        // … and the hook fired exactly once with the just-linked peer's address
        // (so the mail impl can connect to it and provision the mailbox).
        assert_eq!(*hook.calls.lock().unwrap(), vec!["https://peer.test"]);
        // Clean finish, no error.
        let snap = m.snapshot();
        assert!(snap.error.is_none());
        assert_eq!(snap.status, LinkedNestStatus::Idle);
    }

    #[tokio::test]
    async fn link_both_without_hook_does_not_require_one() {
        // The plain `new` constructor wires no hook — `LinkBoth` still seeds both
        // rows (back-compat: content-only multi-homing, web, single-end clients).
        let peer = Arc::new(FakeNest::with_id(0xbb, "https://peer.test"));
        let connected = Arc::new(FakeNest::with_id(0xaa, "https://this.test"));
        *connected.peer.lock().unwrap() = Some(peer.clone());
        let m = LinkedNestsMachine::new(connected.clone());
        m.dispatch(LinkedNestsAction::LinkBoth {
            other_nest_url: "https://peer.test".into(),
            capabilities: vec![],
            expires_at: None,
            label: None,
        })
        .await
        .unwrap();
        assert_eq!(connected.state.lock().unwrap().added.len(), 1);
        assert_eq!(peer.state.lock().unwrap().added.len(), 1);
    }

    #[tokio::test]
    async fn link_both_hook_error_surfaces_but_leaves_link_written_and_visible() {
        let hook = Arc::new(FakePostLinkHook {
            fail: true,
            ..Default::default()
        });
        let (m, connected, peer) = link_both_machine_with_hook(hook.clone());
        let err = m
            .dispatch(LinkedNestsAction::LinkBoth {
                other_nest_url: "https://peer.test".into(),
                capabilities: vec![],
                expires_at: None,
                label: Some("home box".into()),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, PairDispatchError::Nest(_)));
        // The link is durable — both rows were written before the hook ran.
        assert_eq!(connected.state.lock().unwrap().added.len(), 1);
        assert_eq!(peer.state.lock().unwrap().added.len(), 1);
        // The pairing stays visible (refresh ran before the hook) and the hook
        // error is surfaced for the user to retry (idempotent re-run).
        let snap = m.snapshot();
        assert_eq!(snap.pairings.len(), 1);
        assert!(snap.error.is_some());
        assert_eq!(snap.status, LinkedNestStatus::Idle);
    }

    // ── Link-input classification (shared URL-vs-identity routing) ───────────

    #[test]
    fn classify_link_input_routes_64_hex_to_nest_id() {
        let id = "ab".repeat(32);
        assert_eq!(
            classify_link_input(&id),
            LinkInput::NestId {
                nest_id: id.clone()
            }
        );
        // Surrounding whitespace is trimmed.
        assert_eq!(
            classify_link_input(&format!("  {id}\n")),
            LinkInput::NestId { nest_id: id }
        );
    }

    #[test]
    fn classify_link_input_routes_addresses_to_nest_url() {
        for raw in [
            "https://pub.example",
            "https://nas.lan:8443",
            "pub.example",
            "ab",                            // too short for an id
            &"cd".repeat(20),                // 40 hex chars — not 64
            &format!("g{}", "a".repeat(63)), // 64 chars but not all hex
        ] {
            match classify_link_input(raw) {
                LinkInput::NestUrl { nest_url } => assert_eq!(nest_url, raw.trim()),
                other => panic!("{raw:?} should classify as NestUrl, got {other:?}"),
            }
        }
    }

    // ── Trust facet (Nests page) ────────────────────────────────────────────
    //
    // The machine drives Mint/Renew/Revoke/SetLens over the `FakeNest` seam
    // (which captures the capability RPCs + holds a content-processor roster)
    // plus fake config-store / signer / platform seams. These pin the wiring: a
    // mint deposits a sealed blob AND records a signed event AND surfaces the
    // grant in the snapshot; revoke drops it from Now but keeps History; a
    // pairing-only machine has no home row + rejects trust actions.

    use ed25519_dalek::SigningKey;
    use fauna_core::grant_event::{GrantEvent, GrantEventKind};
    use fauna_core::identity::{ActorId, ActorKeypair};

    const NOW: u64 = 1_700_000_000;

    /// Seed a signed `Mint` into a test config's grant events — the log
    /// [`trust_store`] seeds the ledger double from.
    #[allow(clippy::too_many_arguments)]
    fn record_mint_cfg(
        cfg: &mut fauna_core::succession_ledger::SuccessionLedger,
        key: &ed25519_dalek::SigningKey,
        grant_id: [u8; 16],
        holder: [u8; 32],
        scope: Vec<fauna_core::grant_event::GrantEventScope>,
        window_start: u64,
        window_end: u64,
        at: u64,
    ) -> Result<GrantEvent, fauna_core::grant_event::GrantEventError> {
        let mut log = fauna_core::succession_ledger::SuccessionLedger::empty(cfg.actor_id);
        let event = grant_log::record_mint(
            &mut log,
            key,
            grant_id,
            holder,
            scope,
            window_start,
            window_end,
            at,
        )?;
        cfg.grant_events.push(event.clone());
        Ok(event)
    }

    /// [`record_mint_cfg`]'s `Revoke` twin.
    fn record_revoke_cfg(
        cfg: &mut fauna_core::succession_ledger::SuccessionLedger,
        key: &ed25519_dalek::SigningKey,
        grant_id: [u8; 16],
        holder: [u8; 32],
        at: u64,
    ) -> Result<GrantEvent, fauna_core::grant_event::GrantEventError> {
        let mut log = fauna_core::succession_ledger::SuccessionLedger::empty(cfg.actor_id);
        let event = grant_log::record_revoke(&mut log, key, grant_id, holder, at)?;
        cfg.grant_events.push(event.clone());
        Ok(event)
    }

    fn trust_cfg(owner: ActorId) -> fauna_core::succession_ledger::SuccessionLedger {
        fauna_core::succession_ledger::SuccessionLedger::empty(owner)
    }

    /// A shared, ordered call trace — the only way to observe that the
    /// reconcile sweep enumerates *before* it reads the grant log, which is the
    /// sweep's whole correctness argument (`nests.md`: record-then-deposit makes
    /// an honest-nest false orphan impossible only in that order).
    type CallTrace = Arc<StdMutex<Vec<&'static str>>>;

    /// The trust facet's store: the grant-event log
    /// (`fauna.state.succession-ledger`).
    struct TrustStores {
        ledger: Arc<fauna_client_config::test_helpers::FakeSuccessionLedgerStore>,
    }

    /// The ledger seam with every `load` pushed onto the shared [`CallTrace`],
    /// so the grant log's reads interleave with the nest double's calls in one
    /// ordered log. Everything else is the wrapped store's.
    struct TracedLedger {
        inner: Arc<fauna_client_config::test_helpers::FakeSuccessionLedgerStore>,
        trace: CallTrace,
    }

    #[async_trait::async_trait]
    impl SuccessionLedgerStore for TracedLedger {
        fn self_actor(&self) -> Result<ActorId, fauna_client_config::StoreError> {
            self.inner.self_actor()
        }

        async fn load(
            &self,
        ) -> Result<fauna_core::succession_ledger::SuccessionLedger, fauna_client_config::StoreError>
        {
            self.trace.lock().unwrap().push("ledger-load");
            self.inner.load().await
        }

        async fn merge(
            &self,
            replica: fauna_core::succession_ledger::SuccessionLedger,
        ) -> Result<fauna_core::succession_ledger::SuccessionLedger, fauna_client_config::StoreError>
        {
            self.inner.merge(replica).await
        }

        async fn repoint(&self, retired: ActorId) -> Result<bool, fauna_client_config::StoreError> {
            self.inner.repoint(retired).await
        }

        async fn raise_grant_marks(
            &self,
            predecessor: ActorId,
        ) -> Result<bool, fauna_client_config::StoreError> {
            self.inner.raise_grant_marks(predecessor).await
        }
    }

    /// Build the doubles over `cfg` — the ledger seeded from the grant events a
    /// test recorded into it.
    fn trust_store(cfg: fauna_core::succession_ledger::SuccessionLedger) -> Arc<TrustStores> {
        let ledger = fauna_core::succession_ledger::SuccessionLedger {
            grant_events: cfg.grant_events.clone(),
            ..fauna_core::succession_ledger::SuccessionLedger::empty(cfg.actor_id)
        };
        Arc::new(TrustStores {
            ledger: Arc::new(
                fauna_client_config::test_helpers::FakeSuccessionLedgerStore::with(ledger),
            ),
        })
    }

    /// The grant-event log as currently stored, in chronological order (the
    /// ledger keeps its canonical byte order; `[0]` / `.last()` here mean the
    /// first and the latest event) — `at`, then the lifecycle's causal order.
    fn stored_events(store: &TrustStores) -> Vec<GrantEvent> {
        let rank = |k: GrantEventKind| match k {
            GrantEventKind::Mint => 0,
            GrantEventKind::Renew => 1,
            GrantEventKind::Revoke => 2,
        };
        let mut events = store.ledger.current().grant_events;
        events.sort_by_key(|e| (e.at, rank(e.kind)));
        events
    }

    /// Stand-in for the per-app glue's signer — holds the raw key (in the real
    /// seam it never crosses the FFI boundary; here it's a test fixture).
    struct FakeSigner {
        key: SigningKey,
    }
    impl GrantEventSigner for FakeSigner {
        fn sign_grant_event(&self, event: GrantEvent) -> Result<GrantEvent, TrustSignerError> {
            event
                .sign(&self.key)
                .map_err(|e| TrustSignerError::Sign(e.to_string()))
        }
    }

    /// Deterministic clock + grant-id source (a settable `now` and a counter).
    /// The blessing door, in memory — the plane impl's semantics minus the
    /// stamp (re-asserting the standing verdict changes nothing).
    #[derive(Default)]
    struct FakeBlessings(StdMutex<std::collections::BTreeMap<[u8; 32], bool>>);

    #[async_trait::async_trait]
    impl BlessedNestsStore for FakeBlessings {
        async fn is_blessed(&self, nest_id: &[u8; 32]) -> Result<bool, String> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(nest_id)
                .copied()
                .unwrap_or(false))
        }

        async fn set_blessed(
            &self,
            nest_id: &[u8; 32],
            blessed: bool,
            _now: u64,
        ) -> Result<bool, String> {
            Ok(self.0.lock().unwrap().insert(*nest_id, blessed) != Some(blessed))
        }
    }

    struct FakePlatform {
        now: StdMutex<u64>,
        next_id: StdMutex<u8>,
    }
    impl FakePlatform {
        fn set_now(&self, n: u64) {
            *self.now.lock().unwrap() = n;
        }
    }
    impl TrustPlatform for FakePlatform {
        fn now_epoch_secs(&self) -> u64 {
            *self.now.lock().unwrap()
        }
        fn new_grant_id(&self) -> [u8; 16] {
            let mut n = self.next_id.lock().unwrap();
            let id = [*n; 16];
            *n = n.wrapping_add(1);
            id
        }
    }

    fn mda_holder() -> HolderInfo {
        HolderInfo {
            pubkey: [7u8; 32],
            mlkem_ek: None, // classical wrap; these wiring tests use a keyless scope
            role: "mda".into(),
            bridge_id: "mda-1".into(),
        }
    }

    /// A second content-processor holder sharing the generic
    /// `"content-processor"` role with any future sibling (e.g. the web-serve
    /// paywall holder) — distinct `bridge_id`/`pubkey` from [`mda_holder`], for
    /// the holder-targeting regression below.
    fn web_serve_holder() -> HolderInfo {
        HolderInfo {
            pubkey: [9u8; 32],
            mlkem_ek: None,
            role: "content-processor".into(),
            bridge_id: "web-serve".into(),
        }
    }

    /// A keyless scope (`derive_scope_payload` → `None`), so these wiring tests
    /// stay independent of the MSEK-derived key material (that crypto is covered
    /// in `fauna-client-capabilities`).
    fn label_write_scope() -> TrustScope {
        TrustScope {
            class: "content.label-write".into(),
            kind: None,
            tier: None,
        }
    }

    /// A mail custody whose mail subsystem is enabled — i.e. one whose MSEK
    /// lets `derive_scope_payload` answer for `content.read{mail}` and
    /// `content.read{calendar}`, which is what makes `mint_options` emit the two
    /// options the one-tap default set is built from. The default custody
    /// derives nothing, which is the "catalog is empty" case tested separately.
    fn mail_enabled() -> fauna_core::data::MailConfig {
        fauna_core::data::MailConfig {
            msek: Some(fauna_core::secret::SecretArray32::from([3u8; 32])),
            ..Default::default()
        }
    }

    // ── backup trust rows (`nest-trust-backup-item`) ────────────────

    /// A fake source/destination backup seam. One type serves both ends, the
    /// same way the real `BackupNestSeam` does; `fail_writer_list` drives the
    /// unreachable path.
    #[derive(Default)]
    struct FakeBackupNest {
        enrolled: bool,
        writer_grants: Vec<fauna_protocol::backup::WriterGrantItem>,
        fail_writer_list: bool,
        /// The destination answers `revoked: false` even for a grant it still
        /// lists — a revoke it did not perform.
        revoke_answers_false: bool,
        seal_revoked: StdMutex<bool>,
        writer_revoked: StdMutex<Vec<String>>,
        // ── generation recovery (`nest-trust-generation-item`) ───────
        /// What `generation.list` answers, and the window it reports. Note the
        /// two failure switches are separate: a destination can serve the trust
        /// facet's writer list and still fail the generation read, which is the
        /// per-row degrade the facet must survive.
        generations: Vec<fauna_protocol::backup::GenerationItem>,
        grace_secs: i64,
        fail_generation_list: bool,
        /// Manifest hashes this destination will actually restore. Anything
        /// else answers `restored: false` — the past-`T`/unknown case.
        restorable: Vec<String>,
        restored: StdMutex<Vec<(String, String, String)>>,
    }

    /// The home nest's hex identity as its connection proved it —
    /// [`FakeNest::default`]'s `self_id`, the writer id every backup trust row
    /// and writer revoke keys on.
    const BACKUP_SOURCE_ID: &str =
        "0101010101010101010101010101010101010101010101010101010101010101";
    /// A sibling nest's id — what a hostile home nest claims to be.
    const BACKUP_SIBLING_ID: &str =
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn writer_grant(writer: &str, granted_at: i64) -> fauna_protocol::backup::WriterGrantItem {
        fauna_protocol::backup::WriterGrantItem {
            writer_nest_id: writer.into(),
            granted_at,
            ..Default::default()
        }
    }

    #[async_trait]
    impl fauna_client_backup::trust::BackupNestSeam for FakeBackupNest {
        async fn status(&self) -> Result<fauna_protocol::backup::BackupStatusReply, String> {
            Ok(fauna_protocol::backup::BackupStatusReply {
                enrolled: self.enrolled,
                destinations: vec![],
                extra: Default::default(),
            })
        }
        async fn nest_key_revoke(&self) -> Result<(), String> {
            *self.seal_revoked.lock().unwrap() = true;
            Ok(())
        }
        async fn writer_grant_list(
            &self,
        ) -> Result<fauna_protocol::backup::WriterGrantListReply, String> {
            if self.fail_writer_list {
                return Err("destination refused".into());
            }
            Ok(fauna_protocol::backup::WriterGrantListReply {
                grants: self.writer_grants.clone(),
                extra: Default::default(),
            })
        }
        async fn writer_grant_revoke(&self, writer_nest_id: String) -> Result<bool, String> {
            let held = self
                .writer_grants
                .iter()
                .any(|g| g.writer_nest_id.eq_ignore_ascii_case(&writer_nest_id));
            self.writer_revoked.lock().unwrap().push(writer_nest_id);
            // A destination holding no grant for the id answers `false` — the
            // unknown-id and the retry-after-lost-ack cases alike;
            // `revoke_answers_false` forces it while the grant stays listed.
            Ok(held && !self.revoke_answers_false)
        }
        async fn custody_list(
            &self,
            _cursor: Option<String>,
        ) -> Result<fauna_protocol::backup::CustodyListReply, String> {
            // The trust facet reads grants; custody is the audit loop's read
            // (`fauna_client_backup::audit`). No trust-row test reaches it.
            panic!("the trust facet does not read custody")
        }
        async fn generation_list(
            &self,
            _cursor: Option<String>,
        ) -> Result<fauna_protocol::backup::GenerationListReply, String> {
            if self.fail_generation_list {
                return Err("destination refused".into());
            }
            // One complete reply, no `next_cursor` — the walk drains in a page.
            Ok(fauna_protocol::backup::GenerationListReply {
                generations: self.generations.clone(),
                grace_secs: self.grace_secs,
                next_cursor: None,
                extra: Default::default(),
            })
        }
        async fn generation_restore(
            &self,
            folder_name: String,
            path_hash: String,
            manifest_hash: String,
        ) -> Result<fauna_protocol::backup::GenerationRestoreReply, String> {
            self.restored.lock().unwrap().push((
                folder_name,
                path_hash.clone(),
                manifest_hash.clone(),
            ));
            Ok(fauna_protocol::backup::GenerationRestoreReply {
                restored: self.restorable.contains(&manifest_hash),
                extra: Default::default(),
            })
        }
    }

    struct FakeConnector {
        destination: Arc<FakeBackupNest>,
        /// Every URL the connector was asked for — proves the revoke dialed the
        /// destination named by the *client's own* config.
        dialed: StdMutex<Vec<String>>,
    }

    #[async_trait]
    impl fauna_client_backup::trust::BackupDestinationConnector for FakeConnector {
        async fn connect(
            &self,
            url: &str,
        ) -> Result<fauna_client_backup::trust::DestinationConnection, String> {
            self.dialed.lock().unwrap().push(url.to_string());
            Ok(fauna_client_backup::trust::DestinationConnection {
                seam: self.destination.clone(),
                // The id every `backup_dest` row enrolled.
                bound_nest_id: [9u8; 32],
            })
        }
    }

    fn backup_dest(id: &str, url: &str) -> fauna_core::data::BackupDestination {
        fauna_core::data::BackupDestination {
            destination_id: id.into(),
            destination_nest_url: url.into(),
            destination_actor_pubkey: [9u8; 32],
            folder_name: "__mail".into(),
            added_at: 0,
            display_name: Some("Aunt's nest".into()),
            ..Default::default()
        }
    }

    /// A trust machine whose facet also carries the backup half.
    fn backup_trust_machine(
        nest: Arc<FakeNest>,
        kp: &ActorKeypair,
        source: Arc<FakeBackupNest>,
        destination: Arc<FakeBackupNest>,
        destinations: Vec<fauna_core::data::BackupDestination>,
    ) -> (LinkedNestsMachine, Arc<FakeConnector>) {
        let store = trust_store(trust_cfg(kp.actor_id()));
        // The list rests under the box the home connection proves — the one
        // the facet reads (`fauna.state.backup`, per source box).
        let backup_state = fauna_client_config::test_helpers::FakeBackupStateStore::empty();
        let bound: [u8; 32] = nest.self_id.clone().try_into().expect("a 32-byte id");
        backup_state.seed_list(bound, destinations);
        let connector = Arc::new(FakeConnector {
            destination,
            dialed: StdMutex::new(Vec::new()),
        });
        let seams = TrustSeams {
            actor_id: kp.actor_id().0,
            ledger: store.ledger.clone(),
            period_keys: fauna_client_subscriptions::period_keys::MemoryPeriodKeyStore::new()
                .shared(),
            mail: Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
            signer: Arc::new(FakeSigner {
                key: kp.signing_key().clone(),
            }),
            platform: Arc::new(FakePlatform {
                now: StdMutex::new(NOW),
                next_id: StdMutex::new(1),
            }),
            blessings: Arc::new(FakeBlessings::default()),
            backup: Some(BackupTrustSeams {
                state: Arc::new(backup_state),
                source,
                connector: connector.clone(),
            }),
            folder_names: None,
        };
        let machine = LinkedNestsMachine::new_with_trust(nest as Arc<dyn LinkedNestsNest>, seams);
        (machine, connector)
    }

    #[tokio::test]
    async fn the_home_row_carries_the_seal_and_writer_backup_rows() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().pairings.push(row(0xab));
        let source = Arc::new(FakeBackupNest {
            enrolled: true,
            ..Default::default()
        });
        let destination = Arc::new(FakeBackupNest {
            writer_grants: vec![fauna_protocol::backup::WriterGrantItem {
                writer_nest_id: BACKUP_SOURCE_ID.into(),
                granted_at: 1_700_000_000,
                ..Default::default()
            }],
            ..Default::default()
        });
        let (m, _conn) = backup_trust_machine(
            nest,
            &kp,
            source,
            destination,
            vec![backup_dest("d1", "https://aunt.example")],
        );
        m.hydrate().await.unwrap();
        let snap = m.snapshot();

        let home = snap.home.expect("home row");
        assert_eq!(home.trust_backups.len(), 2);
        assert_eq!(home.trust_backups[0].kind, TrustBackupKind::Seal);
        assert_eq!(home.trust_backups[0].status, TrustBackupStatus::Active);
        assert_eq!(home.trust_backups[1].kind, TrustBackupKind::Writer);
        assert_eq!(home.trust_backups[1].destination_id, "d1");
        assert_eq!(home.trust_backups[1].destination_label, "Aunt's nest");
        assert_eq!(home.trust_backups[1].since, Some(1_700_000_000));
        // Both grants empower the SOURCE nest, so no pairing row carries one.
        assert!(snap.pairings.iter().all(|p| p.trust_backups.is_empty()));
    }

    #[tokio::test]
    async fn a_failing_backup_read_never_blanks_the_nests_page() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().pairings.push(row(0xab));
        let source = Arc::new(FakeBackupNest {
            enrolled: true,
            ..Default::default()
        });
        let destination = Arc::new(FakeBackupNest {
            fail_writer_list: true,
            ..Default::default()
        });
        let (m, _conn) = backup_trust_machine(
            nest,
            &kp,
            source,
            destination,
            vec![backup_dest("d1", "https://aunt.example")],
        );
        m.hydrate().await.unwrap();
        let snap = m.snapshot();

        assert_eq!(snap.status, LinkedNestStatus::Idle);
        assert_eq!(snap.pairings.len(), 1, "the page still lists pairings");
        let home = snap.home.expect("home row");
        assert_eq!(
            home.trust_backups[1].status,
            TrustBackupStatus::Unreachable,
            "a destination we could not ask is unreachable, never missing"
        );
    }

    // ── generation recovery (`nest-trust-generation-item`) ──────────

    /// A retained generation as the destination would report it.
    fn generation_item(
        path: Option<&str>,
        path_hash: &str,
        manifest_hash: &str,
        superseded_at: i64,
    ) -> fauna_protocol::backup::GenerationItem {
        fauna_protocol::backup::GenerationItem {
            folder_name: "__mail".into(),
            path: path.map(Into::into),
            path_hash: path_hash.into(),
            manifest_hash: manifest_hash.into(),
            size_bytes: 4096,
            superseded_at,
            extra: Default::default(),
        }
    }

    #[tokio::test]
    async fn the_home_row_lists_retained_generations_with_the_destinations_own_window() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().pairings.push(row(0xab));
        let source = Arc::new(FakeBackupNest {
            enrolled: true,
            ..Default::default()
        });
        let destination = Arc::new(FakeBackupNest {
            // Newest supersede first — the destination's order, which the
            // projection preserves rather than re-sorting (`nests.md:117`).
            generations: vec![
                generation_item(Some("/Mail/2026"), "aa11", "mm11", 1_700_000_000),
                generation_item(Some("/Mail/2025"), "bb22", "mm22", 1_600_000_000),
            ],
            // Deliberately NOT 30 d: a client that hard-codes `T` would render
            // the wrong deadline the moment a destination ran a different
            // window, so the assertion below is what pins `nests.md:118`.
            grace_secs: 86_400,
            ..Default::default()
        });
        let (m, _conn) = backup_trust_machine(
            nest,
            &kp,
            source,
            destination,
            vec![backup_dest("d1", "https://aunt.example")],
        );
        m.hydrate().await.unwrap();
        let snap = m.snapshot();

        let home = snap.home.expect("home row");
        assert_eq!(home.trust_generations.len(), 2);
        let first = &home.trust_generations[0];
        assert_eq!(first.status, TrustGenerationStatus::Listed);
        assert_eq!(first.destination_id, "d1");
        assert_eq!(first.destination_label, "Aunt's nest");
        assert_eq!(first.path.as_deref(), Some("/Mail/2026"));
        assert_eq!(first.manifest_hash, "mm11");
        assert_eq!(
            first.expires_at,
            1_700_000_000 + 86_400,
            "expires_at is superseded_at + the window the DESTINATION reported"
        );
        assert_eq!(home.trust_generations[1].manifest_hash, "mm22");
        // Recovery is addressed to the owner's destinations, which hang off the
        // home row — never off a pairing.
        assert!(snap.pairings.iter().all(|p| p.trust_generations.is_empty()));
    }

    /// Invariant 1 (`nests.md:122`) — the load-bearing one. "We could not ask"
    /// must never render as "there is nothing to recover".
    #[tokio::test]
    async fn an_unreachable_destination_renders_a_row_not_an_absence() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        let source = Arc::new(FakeBackupNest {
            enrolled: true,
            ..Default::default()
        });
        let destination = Arc::new(FakeBackupNest {
            fail_generation_list: true,
            ..Default::default()
        });
        let (m, _conn) = backup_trust_machine(
            nest,
            &kp,
            source,
            destination,
            vec![backup_dest("d1", "https://aunt.example")],
        );
        m.hydrate().await.unwrap();
        let snap = m.snapshot();

        let home = snap.home.expect("home row");
        assert_eq!(
            home.trust_generations.len(),
            1,
            "a destination that could not be asked contributes exactly one row"
        );
        let row = &home.trust_generations[0];
        assert_eq!(row.status, TrustGenerationStatus::Unreachable);
        assert_eq!(
            row.destination_id, "d1",
            "an unreachable row still names WHICH destination went dark"
        );
        assert!(
            row.manifest_hash.is_empty() && row.path_hash.is_empty(),
            "it carries no restore address — a shell renders no restore affordance"
        );
        // And the page still stands: a dead generation read is not allowed to
        // blank the trust rows beside it.
        assert!(snap.error.is_none());
    }

    /// Invariant 2 (`nests.md:123`) — a custody row with no plaintext `path` (the S9
    /// scrub nulls it beside `path_sealed`; a non-conforming source may omit it)
    /// has none to show, and the rows a rogue source
    /// produced are exactly the ones a user needs to see.
    #[tokio::test]
    async fn a_path_less_generation_is_still_listed_with_its_hash() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        let source = Arc::new(FakeBackupNest {
            enrolled: true,
            ..Default::default()
        });
        let destination = Arc::new(FakeBackupNest {
            generations: vec![generation_item(None, "bb22", "mm22", 1_600_000_000)],
            grace_secs: 2_592_000,
            ..Default::default()
        });
        let (m, _conn) = backup_trust_machine(
            nest,
            &kp,
            source,
            destination,
            vec![backup_dest("d1", "https://aunt.example")],
        );
        m.hydrate().await.unwrap();
        let snap = m.snapshot();

        let home = snap.home.expect("home row");
        assert_eq!(
            home.trust_generations.len(),
            1,
            "a path-less row is shown, never hidden or skipped"
        );
        assert!(home.trust_generations[0].path.is_none());
        assert_eq!(
            home.trust_generations[0].path_hash, "bb22",
            "the hash is what the shell renders in the path leaf's place"
        );
    }

    #[tokio::test]
    async fn restoring_a_generation_dials_the_destination_and_carries_the_row_address() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        let source = Arc::new(FakeBackupNest {
            enrolled: true,
            ..Default::default()
        });
        let destination = Arc::new(FakeBackupNest {
            generations: vec![generation_item(
                Some("/Mail/2026"),
                "aa11",
                "mm11",
                1_700_000_000,
            )],
            grace_secs: 2_592_000,
            restorable: vec!["mm11".into()],
            ..Default::default()
        });
        let (m, conn) = backup_trust_machine(
            nest,
            &kp,
            source.clone(),
            destination.clone(),
            vec![backup_dest("d1", "https://aunt.example")],
        );

        m.dispatch(LinkedNestsAction::RestoreGeneration {
            destination_id: "d1".into(),
            folder_name: "__mail".into(),
            path_hash: "aa11".into(),
            manifest_hash: "mm11".into(),
        })
        .await
        .unwrap();

        assert!(
            conn.dialed
                .lock()
                .unwrap()
                .contains(&"https://aunt.example".to_string()),
            "the destination URL must come from the client's own pinned config"
        );
        assert_eq!(
            destination.restored.lock().unwrap().as_slice(),
            &[("__mail".to_string(), "aa11".to_string(), "mm11".to_string())],
            "the restore lands at the destination carrying the row's own address triple"
        );
        assert!(
            !*source.seal_revoked.lock().unwrap(),
            "a restore must not touch the source nest at all"
        );
        assert_eq!(
            m.snapshot().restore_outcome,
            Some(TrustRestoreOutcome::Restored)
        );
    }

    /// Invariant 3 (`nests.md:124`) — asking too late is a product state, not a
    /// failure. It must not reach `error`, or every shell reports a broken
    /// restore for a generation that simply aged past `T`.
    #[tokio::test]
    async fn a_generation_past_the_window_is_an_outcome_not_an_error() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        let source = Arc::new(FakeBackupNest {
            enrolled: true,
            ..Default::default()
        });
        let destination = Arc::new(FakeBackupNest {
            // Nothing is restorable, so the destination answers `restored: false`.
            restorable: vec![],
            ..Default::default()
        });
        let (m, _conn) = backup_trust_machine(
            nest,
            &kp,
            source,
            destination,
            vec![backup_dest("d1", "https://aunt.example")],
        );

        m.dispatch(LinkedNestsAction::RestoreGeneration {
            destination_id: "d1".into(),
            folder_name: "__mail".into(),
            path_hash: "gone".into(),
            manifest_hash: "gone".into(),
        })
        .await
        .expect("asking too late is not a dispatch failure");

        let snap = m.snapshot();
        assert_eq!(
            snap.restore_outcome,
            Some(TrustRestoreOutcome::PastRecoveryWindow)
        );
        assert!(
            snap.error.is_none(),
            "past-the-window must never surface as an error — different words, \
             because only one of the two is worth retrying"
        );
    }

    #[tokio::test]
    async fn revoking_a_writer_grant_dials_the_destination_from_the_clients_own_config() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        let source = Arc::new(FakeBackupNest {
            enrolled: true,
            ..Default::default()
        });
        let destination = Arc::new(FakeBackupNest::default());
        let (m, conn) = backup_trust_machine(
            nest,
            &kp,
            source.clone(),
            destination.clone(),
            vec![backup_dest("d1", "https://aunt.example")],
        );

        m.dispatch(LinkedNestsAction::RevokeBackupWriter {
            destination_id: "d1".into(),
        })
        .await
        .unwrap();

        assert!(
            conn.dialed
                .lock()
                .unwrap()
                .contains(&"https://aunt.example".to_string()),
            "the destination URL must come from the client's own pinned config"
        );
        assert_eq!(
            destination.writer_revoked.lock().unwrap().as_slice(),
            &[BACKUP_SOURCE_ID.to_string()],
            "the revoke lands at the destination — the hostile-source path"
        );
        assert!(
            !*source.seal_revoked.lock().unwrap(),
            "a writer revoke must not touch the seal grant"
        );
    }

    /// A hostile home nest whose own `fauna.nest.info` names a sibling (an id
    /// it learned from the pairing list) must not steer the backup writer
    /// facet onto that sibling: the row reads the grant held for the id this
    /// connection PROVED, and the revoke withdraws that grant — never the
    /// sibling's legitimate one, which would leave the liar writing. Not a refusal: the revoke is the affordance a
    /// hostile source is revoked BY, so a lie must not be able to block it.
    #[tokio::test]
    async fn a_home_nest_claiming_a_siblings_id_is_revoked_under_the_id_its_connection_proved() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        let sibling = decode_hex32(BACKUP_SIBLING_ID).unwrap();
        *nest.claimed_id.lock().unwrap() = Some(sibling);
        let source = Arc::new(FakeBackupNest {
            enrolled: true,
            ..Default::default()
        });
        // The destination authorizes both boxes, at distinguishable times.
        let destination = Arc::new(FakeBackupNest {
            writer_grants: vec![
                writer_grant(BACKUP_SIBLING_ID, 111),
                writer_grant(BACKUP_SOURCE_ID, 222),
            ],
            ..Default::default()
        });
        let (m, _conn) = backup_trust_machine(
            nest,
            &kp,
            source,
            destination.clone(),
            vec![backup_dest("d1", "https://aunt.example")],
        );

        m.hydrate().await.unwrap();
        let home = m.snapshot().home.expect("home row");
        assert_eq!(
            home.trust_backups[1].since,
            Some(222),
            "the writer row shows the grant held for the proven id, not the claimed sibling's"
        );

        m.dispatch(LinkedNestsAction::RevokeBackupWriter {
            destination_id: "d1".into(),
        })
        .await
        .unwrap();
        assert_eq!(
            destination.writer_revoked.lock().unwrap().as_slice(),
            &[BACKUP_SOURCE_ID.to_string()],
            "the revoke names the bound id — the sibling's grant survives"
        );
    }

    /// `revoked: false` while the destination still lists the grant as held is
    /// a revoke that did not happen — surfaced, never reported as done.
    #[tokio::test]
    async fn a_revoke_the_destination_did_not_perform_is_an_error() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        let source = Arc::new(FakeBackupNest {
            enrolled: true,
            ..Default::default()
        });
        let destination = Arc::new(FakeBackupNest {
            writer_grants: vec![writer_grant(BACKUP_SOURCE_ID, 7)],
            revoke_answers_false: true,
            ..Default::default()
        });
        let (m, _conn) = backup_trust_machine(
            nest,
            &kp,
            source,
            destination,
            vec![backup_dest("d1", "https://aunt.example")],
        );

        let err = m
            .dispatch(LinkedNestsAction::RevokeBackupWriter {
                destination_id: "d1".into(),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, PairDispatchError::InvalidState(_)),
            "a grant still held after the revoke is a failed revoke: {err:?}"
        );
    }

    /// `revoked: false` is ALSO the idempotent answer — a retry after an
    /// unacknowledged success, or a grant already gone. With the destination's
    /// list confirming nothing is held, there was nothing to revoke: no error.
    #[tokio::test]
    async fn a_revoke_with_nothing_left_to_revoke_is_not_an_error() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        let source = Arc::new(FakeBackupNest {
            enrolled: true,
            ..Default::default()
        });
        let destination = Arc::new(FakeBackupNest::default());
        let (m, _conn) = backup_trust_machine(
            nest,
            &kp,
            source,
            destination,
            vec![backup_dest("d1", "https://aunt.example")],
        );

        m.dispatch(LinkedNestsAction::RevokeBackupWriter {
            destination_id: "d1".into(),
        })
        .await
        .unwrap();
        let snap = m.snapshot();
        assert!(snap.error.is_none(), "nothing to revoke is not a failure");
        assert_eq!(
            snap.home.expect("home row").trust_backups[1].status,
            TrustBackupStatus::Missing
        );
    }

    #[tokio::test]
    async fn revoking_the_seal_grant_speaks_to_the_home_nest() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        let source = Arc::new(FakeBackupNest {
            enrolled: true,
            ..Default::default()
        });
        let destination = Arc::new(FakeBackupNest::default());
        let (m, _conn) =
            backup_trust_machine(nest, &kp, source.clone(), destination.clone(), vec![]);

        m.dispatch(LinkedNestsAction::RevokeBackupSeal)
            .await
            .unwrap();

        assert!(*source.seal_revoked.lock().unwrap());
        assert!(destination.writer_revoked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_backup_action_on_a_machine_without_backup_seams_errors_cleanly() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        let (m, _store, _platform) = trust_machine(nest, &kp);

        let err = m
            .dispatch(LinkedNestsAction::RevokeBackupSeal)
            .await
            .expect_err("no backup seams ⇒ InvalidState, never a panic");
        assert!(matches!(err, PairDispatchError::InvalidState(_)), "{err:?}");
    }

    /// Build a trust-enabled machine over `nest`, returning it plus the fake
    /// config store (to read the signed log) and platform (to advance the clock).
    fn trust_machine(
        nest: Arc<FakeNest>,
        kp: &ActorKeypair,
    ) -> (LinkedNestsMachine, Arc<TrustStores>, Arc<FakePlatform>) {
        trust_machine_with_cfg(nest, kp, trust_cfg(kp.actor_id()))
    }

    /// [`trust_machine`] over a caller-supplied config.
    fn trust_machine_with_cfg(
        nest: Arc<FakeNest>,
        kp: &ActorKeypair,
        cfg: fauna_core::succession_ledger::SuccessionLedger,
    ) -> (LinkedNestsMachine, Arc<TrustStores>, Arc<FakePlatform>) {
        trust_machine_traced(nest, kp, cfg, Default::default(), None)
    }

    /// [`trust_machine`] over a mail custody the mint catalog can actually
    /// derive from ([`mail_enabled`]) — the one-tap default-set tests.
    fn trust_machine_with_mail(
        nest: Arc<FakeNest>,
        kp: &ActorKeypair,
    ) -> (LinkedNestsMachine, Arc<TrustStores>, Arc<FakePlatform>) {
        trust_machine_traced(nest, kp, trust_cfg(kp.actor_id()), mail_enabled(), None)
    }

    /// [`trust_machine_with_cfg`] with an optional shared [`CallTrace`] wired
    /// into both the nest seam and the ledger store — the reconcile-ordering pin.
    fn trust_machine_traced(
        nest: Arc<FakeNest>,
        kp: &ActorKeypair,
        cfg: fauna_core::succession_ledger::SuccessionLedger,
        mail: fauna_core::data::MailConfig,
        trace: Option<CallTrace>,
    ) -> (LinkedNestsMachine, Arc<TrustStores>, Arc<FakePlatform>) {
        nest.state.lock().unwrap().trace = trace.clone();
        let store = trust_store(cfg);
        let ledger: Arc<dyn SuccessionLedgerStore> = match trace {
            Some(trace) => Arc::new(TracedLedger {
                inner: store.ledger.clone(),
                trace,
            }),
            None => store.ledger.clone(),
        };
        let platform = Arc::new(FakePlatform {
            now: StdMutex::new(NOW),
            next_id: StdMutex::new(1),
        });
        let seams = TrustSeams {
            actor_id: kp.actor_id().0,
            ledger,
            period_keys: fauna_client_subscriptions::period_keys::MemoryPeriodKeyStore::new()
                .shared(),
            mail: Arc::new(fauna_client_config::test_helpers::FakeMailStore::with(
                &mail,
            )),
            signer: Arc::new(FakeSigner {
                key: kp.signing_key().clone(),
            }),
            platform: platform.clone(),
            blessings: Arc::new(FakeBlessings::default()),
            backup: None,
            folder_names: None,
        };
        let machine = LinkedNestsMachine::new_with_trust(nest as Arc<dyn LinkedNestsNest>, seams);
        (machine, store, platform)
    }

    // ── the reconcile sweep (`nests.md` § Trust facet — grants → *Reconcile*) ──
    //
    // The client half of the one admissible owner-side nest read: at every
    // refresh, revoke on the answering nest each grant row the owner's own
    // signed log does not hold live. These pin the machine wiring; the
    // judgement itself is pinned in `fauna_client_capabilities::grant_log`, and
    // the real-wire proof is `conformance_capability_reconcile_sweep_client.rs`.

    /// Seed a signed `Mint` for `grant_id` into `cfg` — a grant the log knows.
    fn logged_mint(
        cfg: &mut fauna_core::succession_ledger::SuccessionLedger,
        kp: &ActorKeypair,
        grant_id: [u8; 16],
    ) {
        record_mint_cfg(
            cfg,
            kp.signing_key(),
            grant_id,
            [7u8; 32],
            vec![fauna_core::grant_event::GrantEventScope {
                class: "label.write".into(),
                kind: None,
                tier: None,
            }],
            NOW,
            NOW + 86_400,
            NOW,
        )
        .expect("seed a Mint event");
    }

    // ── the paywall grant's folder (`nests.md` § Trust facet — grants) ──

    /// A set-name seam answering a fixed list, or `None` (unreadable now).
    struct FixedSetNames(Option<Vec<String>>);

    #[async_trait::async_trait]
    impl fauna_client_capabilities::OwnedSetNames for FixedSetNames {
        async fn owned_set_names(&self) -> Option<Vec<String>> {
            self.0.clone()
        }
    }

    /// The web-serve holder's paywall grants over "premium" (generation 0
    /// revoked, generation 1 live) and over "gone", a set the owner no longer
    /// lists — what the Nests page's trust facet names them by.
    fn paywall_trust_machine(
        sets: Option<FixedSetNames>,
    ) -> (
        LinkedNestsMachine,
        ActorKeypair,
        impl Fn(&str, u32) -> [u8; 16],
    ) {
        let kp = ActorKeypair::generate();
        let secret = *kp.secret_bytes();
        let id = move |set: &str, generation| {
            fauna_client_capabilities::folder_paywall_grant_id(&secret, set, generation)
        };
        let holder = web_serve_holder().pubkey;
        let folder_read = vec![fauna_core::grant_event::GrantEventScope {
            class: "content.read".into(),
            kind: Some("folder".into()),
            tier: None,
        }];
        let mut cfg = trust_cfg(kp.actor_id());
        let k = kp.signing_key();
        let mint = |cfg: &mut fauna_core::succession_ledger::SuccessionLedger, grant_id, at| {
            record_mint_cfg(
                cfg,
                k,
                grant_id,
                holder,
                folder_read.clone(),
                NOW,
                NOW + 86_400,
                at,
            )
            .expect("seed a Mint event");
        };
        mint(&mut cfg, id("premium", 0), NOW - 30);
        record_revoke_cfg(&mut cfg, k, id("premium", 0), holder, NOW - 20).expect("seed a Revoke");
        mint(&mut cfg, id("premium", 1), NOW - 10);
        mint(&mut cfg, id("gone", 0), NOW - 5);

        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![web_serve_holder()];
        let (machine, _store, _plat) = trust_machine_with_cfg(nest, &kp, cfg);
        let machine = match sets {
            Some(sets) => machine.with_folder_names(TrustFolderNames::new(&kp, Arc::new(sets))),
            None => machine,
        };
        (machine, kp, id)
    }

    #[tokio::test]
    async fn the_paywall_grant_and_its_history_name_the_folder() {
        let (machine, _kp, id) =
            paywall_trust_machine(Some(FixedSetNames(Some(vec!["premium".into()]))));
        machine.hydrate().await.unwrap();
        let home = machine.snapshot().home.expect("home row");
        let premium = Some(TrustFolder::Named {
            name: "premium".into(),
        });

        let grant = |want: [u8; 16]| {
            home.trust_grants
                .iter()
                .find(|g| g.grant_id == want)
                .expect("the grant is on the facet")
                .folder
                .clone()
        };
        assert_eq!(grant(id("premium", 1)), premium);
        assert_eq!(grant(id("gone", 0)), Some(TrustFolder::Deleted));
        assert_eq!(
            scope_label_in_folder(&home.trust_grants[0].scope[0], premium.clone()),
            fauna_core::localized::LocalizedText::key_arg(
                "nests.scope_folder",
                "folder",
                "premium"
            ),
            "the shell's label names the folder"
        );

        let spent = id("premium", 0).to_vec();
        let history_of = |kind| {
            home.trust_history
                .iter()
                .find(|h| h.grant_id == spent && h.kind == kind)
                .expect("the event is on the History lens")
                .folder
                .clone()
        };
        assert_eq!(history_of(TrustEventKind::Mint), premium);
        assert_eq!(
            history_of(TrustEventKind::Revoke),
            premium,
            "a Revoke carries no scope, so it is named by its id"
        );
    }

    #[tokio::test]
    async fn without_readable_set_names_no_grant_names_a_folder() {
        for sets in [None, Some(FixedSetNames(None))] {
            let (machine, _kp, _id) = paywall_trust_machine(sets);
            machine.hydrate().await.unwrap();
            let home = machine.snapshot().home.expect("home row");
            assert_eq!(home.trust_grants.len(), 2);
            assert!(
                home.trust_grants.iter().all(|g| g.folder.is_none())
                    && home.trust_history.iter().all(|h| h.folder.is_none()),
                "never a Deleted guessed from names that were never read"
            );
        }
    }

    #[tokio::test]
    async fn refresh_revokes_the_orphan_row_and_leaves_the_live_one() {
        let kp = ActorKeypair::generate();
        let live = [1u8; 16];
        let orphan = [2u8; 16];

        let mut cfg = trust_cfg(kp.actor_id());
        logged_mint(&mut cfg, &kp, live);

        let nest = Arc::new(FakeNest::default());
        // The nest holds both: one the log minted, one it never did — the row
        // stranded by a deposit whose `Mint` never became durable, which is the
        // live stake the ruling exists to heal.
        nest.state.lock().unwrap().nest_grant_rows = vec![live, orphan];
        let (machine, store, _plat) = trust_machine_with_cfg(nest.clone(), &kp, cfg);

        machine.hydrate().await.expect("hydrate runs the sweep");

        assert_eq!(
            nest.state.lock().unwrap().revoked_grants.as_slice(),
            &[orphan],
            "exactly the unrecognized row is revoked, and the revoke lands on \
             the nest that answered"
        );
        assert_eq!(
            stored_events(&store).len(),
            1,
            "the sweep appends NO GrantEvent — only the seeded Mint remains"
        );
        assert!(
            stored_events(&store)
                .iter()
                .all(|e| e.kind != GrantEventKind::Revoke),
            "a Revoke event for an id the log never minted would be \
             nest-influenced content entering the signed log"
        );
    }

    #[tokio::test]
    async fn a_row_resurrected_after_a_revoke_is_swept_again() {
        let kp = ActorKeypair::generate();
        let resurrected = [3u8; 16];

        let mut cfg = trust_cfg(kp.actor_id());
        logged_mint(&mut cfg, &kp, resurrected);
        record_revoke_cfg(&mut cfg, kp.signing_key(), resurrected, [7u8; 32], NOW + 10)
            .expect("seed a Revoke event");
        let events_before = cfg.grant_events.len();

        // A hostile or buggy nest re-inserts the row it was told to delete.
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().nest_grant_rows = vec![resurrected];
        let (machine, store, _plat) = trust_machine_with_cfg(nest.clone(), &kp, cfg);

        machine.hydrate().await.expect("hydrate runs the sweep");

        assert_eq!(
            nest.state.lock().unwrap().revoked_grants.as_slice(),
            &[resurrected],
            "revocation is terminal in the log, so the row is revoked again — \
             every refresh re-narrows the nest"
        );
        assert_eq!(
            stored_events(&store).len(),
            events_before,
            "…and re-revoking still appends nothing to the log"
        );
    }

    /// The ordering that makes the sweep safe: **enumerate, then read the
    /// grant log**. Record-then-deposit makes an honest deposit's `Mint` durable
    /// before its row exists, so a log read taken *after* the enumerate can
    /// only be newer than the row list — which is what makes an honest-nest
    /// false orphan structurally impossible. Read the log first and a grant
    /// minted between the two reads looks like an orphan and gets revoked out
    /// from under the user. The log is the succession ledger
    /// (`fauna.state.succession-ledger`), so the pin traces the ledger seam's
    /// `load`.
    #[tokio::test]
    async fn the_sweep_enumerates_before_it_reads_the_log() {
        let kp = ActorKeypair::generate();
        let trace: CallTrace = Arc::new(StdMutex::new(Vec::new()));
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().nest_grant_rows = vec![[4u8; 16]];
        let (machine, _store, _plat) = trust_machine_traced(
            nest,
            &kp,
            trust_cfg(kp.actor_id()),
            Default::default(),
            Some(Arc::clone(&trace)),
        );

        machine.hydrate().await.expect("hydrate");

        let calls = trace.lock().unwrap().clone();
        let first_reconcile = calls
            .iter()
            .position(|c| *c == "reconcile")
            .expect("the sweep enumerated");
        let first_log_read = calls
            .iter()
            .position(|c| *c == "ledger-load")
            .expect("the sweep read the log");
        assert!(
            first_reconcile < first_log_read,
            "enumerate must precede the grant-log read (got {calls:?})"
        );
    }

    /// A refused `fauna.capabilities.reconcile`. The
    /// sweep degrades to a no-op and the page still hydrates, and the orphans stay exactly as unreachable as they
    /// already were rather than the Nests page blanking.
    #[tokio::test]
    async fn a_refused_reconcile_still_hydrates() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        {
            let mut s = nest.state.lock().unwrap();
            s.reconcile_refuses = true;
            s.nest_grant_rows = vec![[5u8; 16]]; // never seen — the call refuses
        }
        let (machine, store, _plat) = trust_machine(nest.clone(), &kp);

        machine
            .hydrate()
            .await
            .expect("a refused reconcile must not fail the page");

        assert!(
            nest.state.lock().unwrap().revoked_grants.is_empty(),
            "no enumerate ⇒ no revokes"
        );
        assert!(stored_events(&store).is_empty(), "and no log writes");
        assert!(
            machine.snapshot().home.is_some(),
            "the home row still renders"
        );
    }

    /// One failing revoke must not abandon the rest: each is independent, and
    /// the next refresh retries whatever did not land.
    #[tokio::test]
    async fn a_refused_revoke_does_not_abandon_the_sweep() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        {
            let mut s = nest.state.lock().unwrap();
            s.nest_grant_rows = vec![[6u8; 16], [7u8; 16]];
            s.revoke_grant_refuses = true;
        }
        let (machine, _store, _plat) = trust_machine(nest.clone(), &kp);

        machine
            .hydrate()
            .await
            .expect("a refused revoke must not fail the page either");

        assert!(
            nest.state.lock().unwrap().revoked_grants.is_empty(),
            "the fake refused both; the point is that hydrate still succeeded"
        );
    }

    /// A machine built without trust seams (the pairing-only page) has no log to
    /// judge against, so it must not enumerate at all — the read is admissible
    /// only as the sweep's first step.
    #[tokio::test]
    async fn a_pairing_only_machine_never_enumerates() {
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().nest_grant_rows = vec![[8u8; 16]];
        let machine = LinkedNestsMachine::new(nest.clone() as Arc<dyn LinkedNestsNest>);

        machine.hydrate().await.expect("pairing-only hydrate");

        assert!(
            nest.state.lock().unwrap().revoked_grants.is_empty(),
            "no trust seams ⇒ no sweep, and certainly no revokes"
        );
    }

    // `is_content_processor_holder_excludes_mta_and_keyless` moved with the fn
    // to `fauna-client-bridges` (2026-07-19 lift).

    #[tokio::test]
    async fn mint_deposits_blob_records_signed_event_and_shows_active_grant() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder()];
        let (machine, store, _plat) = trust_machine(nest.clone(), &kp);

        machine
            .dispatch(LinkedNestsAction::Mint {
                nest_id: "home".into(),
                holder_bridge_id: "mda-1".into(),
                scope: vec![label_write_scope()],
                duration: Some(TrustGrantDuration::Standard),
            })
            .await
            .unwrap();

        assert_eq!(
            nest.state.lock().unwrap().minted_blobs.len(),
            1,
            "one sealed blob deposited on the nest"
        );
        let events = stored_events(&store);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, GrantEventKind::Mint);
        events[0]
            .verify(&kp.actor_id())
            .expect("event signed by the owner identity");

        let home = machine.snapshot().home.expect("home row");
        assert!(home.is_home);
        assert_eq!(home.trust_grants.len(), 1);
        assert_eq!(home.trust_grants[0].holder, vec![7u8; 32]);
        assert_eq!(home.trust_grants[0].liveness, TrustLiveness::Active);
        assert_eq!(home.trust_grants[0].scope[0].class, "content.label-write");
    }

    /// The one-tap "trust this box" default set (`onboarding.md` § 3b-ter):
    /// **the default set IS the shared mint catalog** — every option
    /// `mint_options` derives, each to its own derived holder, at the standard
    /// window. No second availability rule exists to drift from the picker's.
    ///
    /// A mail-enabled config with an MDA enrolled derives two options — Mail
    /// (`content.read{mail}` bundled with the keyless `content.label-write`,
    /// the production MDA shape) and Calendar — so one tap deposits two sealed
    /// blobs and records two signed events.
    #[tokio::test]
    async fn the_default_set_mints_every_option_the_shared_catalog_derives() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder()];
        let (machine, store, _plat) = trust_machine_with_mail(nest.clone(), &kp);

        machine
            .dispatch(LinkedNestsAction::MintDefaultSet)
            .await
            .unwrap();

        assert_eq!(
            nest.state.lock().unwrap().minted_blobs.len(),
            2,
            "one sealed blob per derivable catalog option (mail, calendar)"
        );
        let events = stored_events(&store);
        assert_eq!(events.len(), 2, "each mint records its own signed event");
        for e in &events {
            assert_eq!(e.kind, GrantEventKind::Mint);
            e.verify(&kp.actor_id())
                .expect("every default-set event is signed by the owner identity");
            assert_eq!(
                e.holder,
                vec![7u8; 32],
                "both options derive the MDA holder — never a blanket mint to \
                 every content processor on the box"
            );
        }

        let home = machine.snapshot().home.expect("home row");
        assert_eq!(
            home.trust_grants.len(),
            2,
            "the Nests page shows the minted grants immediately — the § 3b-ter \
             success surface"
        );
        let mail_grant = home
            .trust_grants
            .iter()
            .find(|g| {
                g.scope
                    .iter()
                    .any(|s| s.kind.as_deref() == Some("mail") && s.class == "content.read")
            })
            .expect("the mail option minted");
        assert!(
            mail_grant
                .scope
                .iter()
                .any(|s| s.class == "content.label-write"),
            "mail is minted in its production bundled shape (read{{mail}} + \
             label-write), never read{{mail}} alone"
        );
    }

    /// The tap is idempotent: a scope already covered by a live grant to that
    /// holder is not minted a second time. The onboarding handoff is
    /// best-effort glue that can re-run (a retried session, a re-entered
    /// wizard), and a duplicate grant is not a no-op — it is a second live
    /// capability the user must revoke twice.
    #[tokio::test]
    async fn the_default_set_skips_what_the_box_already_holds() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder()];
        let (machine, store, _plat) = trust_machine_with_mail(nest.clone(), &kp);

        machine
            .dispatch(LinkedNestsAction::MintDefaultSet)
            .await
            .unwrap();
        machine
            .dispatch(LinkedNestsAction::MintDefaultSet)
            .await
            .unwrap();

        assert_eq!(
            nest.state.lock().unwrap().minted_blobs.len(),
            2,
            "the second tap mints nothing new"
        );
        assert_eq!(
            stored_events(&store).len(),
            2,
            "and records no second event"
        );
    }

    /// **PROBE-359** — the tap deposits option 1, the nest
    /// refuses option 2, and the dispatch fails.
    ///
    /// The invariant is NOT "the batch is atomic": it cannot be, the nest is a
    /// separate machine and the deposit is the commit point. It is the weaker,
    /// achievable one — **whatever the nest ends up holding, the user's app can
    /// see and revoke.** That direction is the whole asymmetry: the Nests page
    /// projects the client log (never the nest), `revoke` needs a `grant_id`
    /// only the log carries, and the wire has no owner-side enumerate — so a
    /// grant on the nest but not in the log is unreachable forever, while a
    /// grant in the log but not on the nest is a phantom row an idempotent
    /// revoke clears.
    ///
    /// On the pre-fix mint-then-record order this printed the review's line
    /// verbatim: `nest holds 1 live grant(s); grant log has 0 event(s); Nests
    /// page shows 0 revocable row(s)`.
    #[tokio::test]
    async fn a_partly_failed_tap_leaves_no_grant_the_user_cannot_revoke() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        {
            let mut s = nest.state.lock().unwrap();
            s.holders = vec![mda_holder()];
            s.mint_accepts_before_refusing = Some(1);
        }
        let (machine, store, _plat) = trust_machine_with_mail(nest.clone(), &kp);

        let outcome = machine.dispatch(LinkedNestsAction::MintDefaultSet).await;
        assert!(
            outcome.is_err(),
            "a refused deposit surfaces to the caller rather than reading as success"
        );

        // What the nest ACTUALLY holds, read back off the wire bytes it
        // accepted — not what the client believes it deposited.
        let held: Vec<Vec<u8>> = nest
            .state
            .lock()
            .unwrap()
            .minted_blobs
            .iter()
            .map(|b| {
                fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(b)
                    .expect("the nest was handed a canonical grant blob")
                    .index
                    .1
            })
            .collect();
        assert_eq!(held.len(), 1, "the fake accepted exactly one deposit");

        // Deliberately not `expect("home row")`: "the page has no row at all"
        // IS one of the failure shapes under test (the pre-fix order left the
        // dispatch short-circuited before any refresh), and it must reach the
        // diagnostic below rather than panicking on an unwrap.
        let rows = machine
            .snapshot()
            .home
            .map(|h| h.trust_grants)
            .unwrap_or_default();
        for id in &held {
            assert!(
                rows.iter().any(|r| &r.grant_id == id),
                "PROBE-359: nest holds {} live grant(s); grant log has {} event(s); \
                 Nests page shows {} revocable row(s) — a capability is live on the \
                 nest that the user's app can neither show nor revoke, and nothing \
                 on the wire can ever discover it",
                held.len(),
                stored_events(&store).len(),
                rows.len()
            );
        }
    }

    /// The page must catch up to the durable log **on the failure path too**.
    /// Recording first is only half the fix: if the error short-circuits the
    /// refresh, the user is told the tap failed while the rows it did record
    /// stay invisible until some later refresh happens to run.
    #[tokio::test]
    async fn a_partly_failed_tap_still_shows_what_it_recorded() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        {
            let mut s = nest.state.lock().unwrap();
            s.holders = vec![mda_holder()];
            s.mint_accepts_before_refusing = Some(1);
        }
        let (machine, store, _plat) = trust_machine_with_mail(nest.clone(), &kp);

        machine
            .dispatch(LinkedNestsAction::MintDefaultSet)
            .await
            .expect_err("the refused deposit fails the dispatch");

        assert_eq!(
            stored_events(&store).len(),
            2,
            "both events were recorded before either blob shipped"
        );
        assert_eq!(
            machine
                .snapshot()
                .home
                .expect("home row")
                .trust_grants
                .len(),
            2,
            "and the page shows them without waiting for a later refresh"
        );
        assert!(
            machine.snapshot().error.is_some(),
            "while still telling the user the tap did not fully succeed"
        );
    }

    /// The **widening** direction, applied to renew: when the nest refuses the
    /// bump, the log has already recorded the longer window. That skew is the
    /// deliberate one — the page over-states what the holder gets, which a
    /// revoke settles. The old nest-first order skewed the other way (a nest
    /// honouring 90 more days behind a page showing the grant lapsed), and a
    /// user does not revoke what they believe already expired.
    #[tokio::test]
    async fn a_refused_renew_leaves_the_log_the_more_permissive_of_the_pair() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder()];
        let (machine, store, plat) = trust_machine(nest.clone(), &kp);
        machine
            .dispatch(LinkedNestsAction::Mint {
                nest_id: "home".into(),
                holder_bridge_id: "mda-1".into(),
                scope: vec![label_write_scope()],
                duration: Some(TrustGrantDuration::Standard),
            })
            .await
            .unwrap();
        let grant_id = stored_events(&store)[0].grant_id.clone();
        let before = machine.snapshot().home.unwrap().trust_grants[0].lasts_until;

        nest.state.lock().unwrap().renew_refuses = true;
        plat.set_now(NOW + 10 * 24 * 60 * 60);
        machine
            .dispatch(LinkedNestsAction::Renew { grant_id })
            .await
            .expect_err("the refused bump surfaces");

        assert_eq!(
            stored_events(&store).len(),
            2,
            "the Renew event was recorded before the nest was asked"
        );
        let after = machine.snapshot().home.unwrap().trust_grants[0].lasts_until;
        assert!(
            after > before,
            "and the page shows the extension it recorded ({after} > {before}), so the \
             grant stays visible and revocable rather than appearing to lapse while the \
             nest quietly honours it"
        );
    }

    /// The **narrowing** direction, and the reason revoke keeps the opposite
    /// order: when the nest refuses the delete, nothing is recorded, so the page
    /// still shows the grant it could not remove. Recording first here would
    /// hide a capability the nest is still honouring — the same failure mode,
    /// just reached by another route.
    #[tokio::test]
    async fn a_refused_revoke_keeps_the_grant_on_the_page() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder()];
        let (machine, store, _plat) = trust_machine(nest.clone(), &kp);
        machine
            .dispatch(LinkedNestsAction::Mint {
                nest_id: "home".into(),
                holder_bridge_id: "mda-1".into(),
                scope: vec![label_write_scope()],
                duration: Some(TrustGrantDuration::Standard),
            })
            .await
            .unwrap();
        let grant_id = stored_events(&store)[0].grant_id.clone();

        nest.state.lock().unwrap().revoke_grant_refuses = true;
        machine
            .dispatch(LinkedNestsAction::Revoke { grant_id })
            .await
            .expect_err("the refused delete surfaces");

        assert_eq!(
            stored_events(&store).len(),
            1,
            "no Revoke event was recorded — the nest still holds the grant"
        );
        assert_eq!(
            machine
                .snapshot()
                .home
                .expect("home row")
                .trust_grants
                .len(),
            1,
            "so the page keeps showing it, and the user can try again"
        );
    }

    /// A box with nothing derivable (no MDA enrolled yet, or mail not enabled)
    /// mints nothing and does **not** fail: the tap is an offer, and an empty
    /// catalog is an honest no-op, not an error banner on the last screen of
    /// onboarding.
    #[tokio::test]
    async fn the_default_set_is_an_honest_no_op_when_the_catalog_is_empty() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        // Mail-enabled config, but no content-processor holder is enrolled.
        let (machine, store, _plat) = trust_machine_with_mail(nest.clone(), &kp);

        machine
            .dispatch(LinkedNestsAction::MintDefaultSet)
            .await
            .expect("nothing to mint is not a dispatch failure");

        assert!(nest.state.lock().unwrap().minted_blobs.is_empty());
        assert!(stored_events(&store).is_empty());
        assert!(
            machine.snapshot().error.is_none(),
            "an empty catalog must not paint an error on the trust prompt"
        );
    }

    /// Regression for the over-broad-grant bug: a nest with TWO
    /// content-processor holders (an MDA and a generic `content-processor`
    /// holder, e.g. web-serve) must mint to ONLY the `holder_bridge_id` named
    /// in the action — never "one grant per holder" (the pre-fix behavior,
    /// which would have handed the web-serve holder a mail-scoped grant it has
    /// no business holding). `LinkedNestsAction::Mint` § doc.
    #[tokio::test]
    async fn mint_targets_only_the_named_holder_when_several_exist() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder(), web_serve_holder()];
        let (machine, store, _plat) = trust_machine(nest.clone(), &kp);

        // Sanity: both holders are discoverable as mint targets.
        machine.hydrate().await.unwrap();
        let home = machine.snapshot().home.expect("home row");
        let mut bridge_ids: Vec<_> = home
            .available_holders
            .iter()
            .map(|h| h.bridge_id.clone())
            .collect();
        bridge_ids.sort();
        assert_eq!(
            bridge_ids,
            vec!["mda-1".to_string(), "web-serve".to_string()]
        );
        drop(home);

        machine
            .dispatch(LinkedNestsAction::Mint {
                nest_id: "home".into(),
                holder_bridge_id: "mda-1".into(),
                scope: vec![label_write_scope()],
                duration: Some(TrustGrantDuration::Standard),
            })
            .await
            .unwrap();

        assert_eq!(
            nest.state.lock().unwrap().minted_blobs.len(),
            1,
            "exactly one blob deposited — NOT one per holder"
        );
        let events = stored_events(&store);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].holder,
            vec![7u8; 32],
            "the grant went to the MDA's pubkey, not the web-serve holder's"
        );

        let home = machine.snapshot().home.expect("home row");
        assert_eq!(
            home.trust_grants.len(),
            1,
            "only the targeted holder shows a grant"
        );
        assert_eq!(home.trust_grants[0].holder, vec![7u8; 32]);
    }

    /// The home row carries the shared mint-picker catalog
    /// (`view_model::mint_options` → FFI-projected `TrustMintOption`s): with an
    /// MDA enrolled and mail enabled (MSEK held), the picker offers Mail
    /// (read{mail} bundled with label-write) and Calendar, each derived to the
    /// single MDA candidate — the scope-first design (`nests.md` § Mint,
    /// ratified 2026-07-13).
    #[tokio::test]
    async fn home_row_carries_mint_options_for_derivable_scopes() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder(), web_serve_holder()];
        let store = trust_store(trust_cfg(kp.actor_id()));
        let platform = Arc::new(FakePlatform {
            now: StdMutex::new(NOW),
            next_id: StdMutex::new(1),
        });
        let seams = TrustSeams {
            actor_id: kp.actor_id().0,
            ledger: store.ledger.clone(),
            period_keys: fauna_client_subscriptions::period_keys::MemoryPeriodKeyStore::new()
                .shared(),
            // Mail enabled (MSEK held).
            mail: Arc::new(fauna_client_config::test_helpers::FakeMailStore::with(
                &mail_enabled(),
            )),
            signer: Arc::new(FakeSigner {
                key: kp.signing_key().clone(),
            }),
            platform,
            blessings: Arc::new(FakeBlessings::default()),
            backup: None,
            folder_names: None,
        };
        let machine = LinkedNestsMachine::new_with_trust(nest as Arc<dyn LinkedNestsNest>, seams);

        machine.hydrate().await.unwrap();
        let home = machine.snapshot().home.expect("home row");
        // No tier custody in this cfg → no PaywalledPosts option, even though a
        // generic content-processor holder is enrolled.
        assert_eq!(home.mint_options.len(), 2, "mail + calendar");
        assert_eq!(home.mint_options[0].use_case, TrustMintUseCase::Mail);
        assert_eq!(
            home.mint_options[0].scope,
            vec![
                TrustScope {
                    class: "content.read".into(),
                    kind: Some("mail".into()),
                    tier: None,
                },
                TrustScope {
                    class: "content.label-write".into(),
                    kind: None,
                    tier: None,
                },
            ],
            "the Mail use case mints the production bundle, never read{{mail}} alone"
        );
        assert_eq!(
            home.mint_options[0].holder_candidates,
            vec!["mda-1".to_string()],
            "single candidate ⇒ the UI derives the holder, no holder-select"
        );
        assert_eq!(home.mint_options[1].use_case, TrustMintUseCase::Calendar);
        assert_eq!(
            home.mint_options[1].holder_candidates,
            vec!["mda-1".to_string()]
        );
    }

    #[tokio::test]
    async fn mint_errors_on_unknown_holder_bridge_id() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder()];
        let (machine, store, _plat) = trust_machine(nest.clone(), &kp);

        let err = machine
            .dispatch(LinkedNestsAction::Mint {
                nest_id: "home".into(),
                holder_bridge_id: "no-such-holder".into(),
                scope: vec![label_write_scope()],
                duration: Some(TrustGrantDuration::Standard),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, PairDispatchError::InvalidState(_)));
        assert!(nest.state.lock().unwrap().minted_blobs.is_empty());
        assert!(stored_events(&store).is_empty());
    }

    #[tokio::test]
    async fn home_row_survives_holder_discovery_error() {
        // Holder discovery (`list_service_users`) is Admin-gated; a non-admin
        // owner's hydrate must still surface the home nest row with an empty
        // trust facet — not fail the whole `refresh`, which would blank the Nests
        // page (pairings are set only after the home row builds). `nests.md`
        // § Implementation status (admin-scoped v1). Regression for the graceful
        // degradation in `build_home_row`.
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders_error = true;
        nest.state.lock().unwrap().pairings.push(row(0xab));
        let (machine, _store, _plat) = trust_machine(nest.clone(), &kp);

        machine
            .hydrate()
            .await
            .expect("holder-discovery failure degrades, not fails hydrate");
        let snap = machine.snapshot();
        assert!(
            snap.error.is_none(),
            "no page error surfaced on discovery failure"
        );
        let home = snap
            .home
            .expect("home row still present for a non-admin owner");
        assert!(home.is_home);
        assert!(
            home.trust_grants.is_empty() && home.trust_history.is_empty(),
            "empty trust facet ⇒ nest-trust-empty"
        );
        assert_eq!(snap.pairings.len(), 1, "pairings still listed");
    }

    #[tokio::test]
    async fn a_ledger_still_assembling_degrades_the_facet_and_refuses_the_write() {
        // Right after sign-in the host's account store is still assembling
        // (minutes on a loaded web tab), and the grant log lives there. The page
        // must still list and link — the facet renders empty, as for a
        // non-admin — while a gesture that writes the log refuses rather than
        // mint a grant it cannot record.
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().pairings.push(row(0xab));
        let (machine, store, _plat) = trust_machine(nest.clone(), &kp);
        store.ledger.set_not_ready(true);

        machine
            .hydrate()
            .await
            .expect("an assembling store degrades the facet, not the page");
        let snap = machine.snapshot();
        assert!(snap.error.is_none(), "no page error: {:?}", snap.error);
        let home = snap.home.expect("the home row still renders");
        assert!(home.trust_grants.is_empty() && home.trust_history.is_empty());
        assert_eq!(snap.pairings.len(), 1, "pairings still listed");

        let err = machine
            .dispatch(LinkedNestsAction::MintDefaultSet)
            .await
            .expect_err("a write to an assembling store refuses");
        assert!(
            err.to_string()
                .contains(fauna_client_config::LEDGER_NOT_READY),
            "{err}"
        );
    }

    #[tokio::test]
    async fn mint_errors_when_nest_has_no_content_processors() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default()); // no holders
        let (machine, store, _plat) = trust_machine(nest.clone(), &kp);

        let err = machine
            .dispatch(LinkedNestsAction::Mint {
                nest_id: "home".into(),
                holder_bridge_id: "mda-1".into(),
                scope: vec![label_write_scope()],
                duration: Some(TrustGrantDuration::Standard),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, PairDispatchError::InvalidState(_)));
        assert!(nest.state.lock().unwrap().minted_blobs.is_empty());
        assert!(stored_events(&store).is_empty());
    }

    #[tokio::test]
    async fn revoke_drops_from_now_keeps_history_and_calls_nest() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder()];
        let (machine, store, _plat) = trust_machine(nest.clone(), &kp);
        machine
            .dispatch(LinkedNestsAction::Mint {
                nest_id: "home".into(),
                holder_bridge_id: "mda-1".into(),
                scope: vec![label_write_scope()],
                duration: Some(TrustGrantDuration::Standard),
            })
            .await
            .unwrap();
        let grant_id = stored_events(&store)[0].grant_id.clone();

        machine
            .dispatch(LinkedNestsAction::Revoke {
                grant_id: grant_id.clone(),
            })
            .await
            .unwrap();

        assert_eq!(nest.state.lock().unwrap().revoked_grants.len(), 1);
        assert_eq!(
            &nest.state.lock().unwrap().revoked_grants[0][..],
            &grant_id[..]
        );
        let home = machine.snapshot().home.unwrap();
        assert!(
            home.trust_grants.is_empty(),
            "revoked ⇒ nest-trust-empty in Now"
        );
        assert_eq!(home.trust_history.len(), 2, "Mint + Revoke kept in History");
        assert_eq!(
            home.trust_history[0].kind,
            TrustEventKind::Revoke,
            "most-recent-first"
        );
    }

    #[tokio::test]
    async fn renew_extends_window_and_calls_nest() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder()];
        let (machine, store, plat) = trust_machine(nest.clone(), &kp);
        machine
            .dispatch(LinkedNestsAction::Mint {
                nest_id: "home".into(),
                holder_bridge_id: "mda-1".into(),
                scope: vec![label_write_scope()],
                duration: Some(TrustGrantDuration::Standard),
            })
            .await
            .unwrap();
        let grant_id = stored_events(&store)[0].grant_id.clone();
        let before = machine.snapshot().home.unwrap().trust_grants[0].lasts_until;

        plat.set_now(NOW + 10 * 24 * 60 * 60); // advance 10 days
        machine
            .dispatch(LinkedNestsAction::Renew {
                grant_id: grant_id.clone(),
            })
            .await
            .unwrap();

        assert_eq!(nest.state.lock().unwrap().renewed.len(), 1);
        let after = machine.snapshot().home.unwrap().trust_grants[0].lasts_until;
        assert!(
            after > before,
            "renew extends the window ({after} > {before})"
        );
        assert_eq!(
            stored_events(&store).last().unwrap().kind,
            GrantEventKind::Renew,
            "a Renew event is logged"
        );
    }

    // ── blessing + auto-renew (`nests.md` § Expiry / renewal → Duration and
    //    blessing) ─────────────────────────────────────────────────────────

    const DAY: u64 = 24 * 60 * 60;

    /// Hydrate and return the home row's hex id — what `SetBlessed` and a
    /// defaulted `Mint` name.
    async fn home_id(machine: &LinkedNestsMachine) -> String {
        machine.hydrate().await.unwrap();
        machine.snapshot().home.unwrap().nest_id
    }

    #[tokio::test]
    async fn a_blessed_nests_due_standard_grant_renews_itself_on_the_tick() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder()];
        let (machine, store, plat) = trust_machine(nest.clone(), &kp);
        let home = home_id(&machine).await;
        machine
            .dispatch(LinkedNestsAction::SetBlessed {
                nest_id: home.clone(),
                blessed: true,
            })
            .await
            .unwrap();
        let row = machine.snapshot().home.unwrap();
        assert!(row.blessed);
        assert_eq!(row.mint_default_duration, TrustGrantDuration::Standard);

        // No duration named: a blessed nest's default is the standard window.
        machine
            .dispatch(LinkedNestsAction::Mint {
                nest_id: home,
                holder_bridge_id: "mda-1".into(),
                scope: vec![label_write_scope()],
                duration: None,
            })
            .await
            .unwrap();
        let grant = &machine.snapshot().home.unwrap().trust_grants[0];
        assert_eq!(grant.lasts_until, (NOW + 90 * DAY) as i64);
        assert_eq!(grant.liveness, TrustLiveness::AutoRenewing);

        // Not yet due: the tick renews nothing.
        machine
            .dispatch(LinkedNestsAction::AutoRenew)
            .await
            .unwrap();
        assert!(nest.state.lock().unwrap().renewed.is_empty());

        // Inside the renew-ahead threshold: the tick renews it, log first.
        plat.set_now(NOW + 80 * DAY);
        machine
            .dispatch(LinkedNestsAction::AutoRenew)
            .await
            .unwrap();
        assert_eq!(nest.state.lock().unwrap().renewed.len(), 1);
        assert_eq!(
            stored_events(&store).last().unwrap().kind,
            GrantEventKind::Renew
        );
        let grant = &machine.snapshot().home.unwrap().trust_grants[0];
        assert_eq!(
            grant.lasts_until,
            (NOW + 80 * DAY + 90 * DAY) as i64,
            "renewed by the grant's own 90-day length from the real now"
        );
    }

    #[tokio::test]
    async fn the_tick_leaves_un_blessed_and_one_off_grants_to_lapse() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder()];
        let (machine, _store, plat) = trust_machine(nest.clone(), &kp);
        let home = home_id(&machine).await;
        assert_eq!(
            machine.snapshot().home.unwrap().mint_default_duration,
            TrustGrantDuration::OneOff,
            "an un-blessed nest defaults to the one-off window"
        );

        // A defaulted mint on the un-blessed nest lasts hours.
        machine
            .dispatch(LinkedNestsAction::Mint {
                nest_id: home.clone(),
                holder_bridge_id: "mda-1".into(),
                scope: vec![label_write_scope()],
                duration: None,
            })
            .await
            .unwrap();
        assert_eq!(
            machine.snapshot().home.unwrap().trust_grants[0].lasts_until,
            (NOW + 8 * 60 * 60) as i64
        );
        // A standard grant the user picked on the un-blessed nest is not the
        // loop's to renew either.
        machine
            .dispatch(LinkedNestsAction::Mint {
                nest_id: home.clone(),
                holder_bridge_id: "mda-1".into(),
                scope: vec![label_write_scope()],
                duration: Some(TrustGrantDuration::Standard),
            })
            .await
            .unwrap();
        plat.set_now(NOW + 80 * DAY);
        machine
            .dispatch(LinkedNestsAction::AutoRenew)
            .await
            .unwrap();
        assert!(nest.state.lock().unwrap().renewed.is_empty(), "un-blessed");

        // Blessing renews the due standard grant at once (the refresh sweeps)
        // but never the one-off, which reads its real liveness.
        plat.set_now(NOW + 60);
        machine
            .dispatch(LinkedNestsAction::Mint {
                nest_id: home.clone(),
                holder_bridge_id: "mda-1".into(),
                scope: vec![label_write_scope()],
                duration: Some(TrustGrantDuration::OneOff),
            })
            .await
            .unwrap();
        plat.set_now(NOW + 80 * DAY);
        machine
            .dispatch(LinkedNestsAction::SetBlessed {
                nest_id: home,
                blessed: true,
            })
            .await
            .unwrap();
        assert_eq!(
            nest.state.lock().unwrap().renewed.len(),
            1,
            "only the standard grant"
        );
        let row = machine.snapshot().home.unwrap();
        assert!(
            row.trust_grants
                .iter()
                .all(|g| g.liveness != TrustLiveness::AutoRenewing
                    || g.lasts_until == (NOW + 170 * DAY) as i64),
            "only the renewed standard grant reads auto-renewing: {:?}",
            row.trust_grants
        );
    }

    #[tokio::test]
    async fn trusting_this_box_in_one_tap_blesses_it() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder()];
        let (machine, _store, _plat) = trust_machine(nest.clone(), &kp);
        machine
            .dispatch(LinkedNestsAction::MintDefaultSet)
            .await
            .unwrap();
        assert!(machine.snapshot().home.unwrap().blessed);
    }

    /// The consent copy the tap sits under must describe what the tap writes
    /// (`onboarding.md` § 3b-ter, honest bound (1)): the tap blesses the box,
    /// so its trust renews itself and the offer may not promise a fixed limit
    /// the blessing removes. Worded for the weakest app — renewal "while you
    /// use Fauna" (no background promise) and revocation, not un-blessing, as
    /// the stop every app renders.
    #[tokio::test]
    async fn the_one_tap_offer_copy_matches_the_blessing_the_tap_writes() {
        use fauna_i18n::strings::onboarding::trust_prompt::SUMMARY;
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder()];
        let (machine, _store, _plat) = trust_machine(nest.clone(), &kp);
        machine
            .dispatch(LinkedNestsAction::MintDefaultSet)
            .await
            .unwrap();
        let blessed = machine.snapshot().home.unwrap().blessed;

        assert!(blessed, "the tap blesses, so the copy below must say so");
        assert!(
            SUMMARY.contains("renews itself while you use Fauna"),
            "offer copy must state the renewal the blessing brings: {SUMMARY}"
        );
        assert!(
            !SUMMARY.contains("90 days"),
            "offer copy must not promise a limit the blessing removes: {SUMMARY}"
        );
        assert!(
            SUMMARY.contains("take it back at any time in Settings → Nests"),
            "the stop the copy names must be one every app renders: {SUMMARY}"
        );
    }

    /// The blessing verdict is keyed on the identity this
    /// connection proved, never on what the nest says about itself. An
    /// un-blessed nest that answers `fauna.nest.info` with a blessed
    /// sibling's id gets no renewal, its row reads un-blessed, and the row's
    /// toggle names the nest itself — so the user's verdict can never land on
    /// the sibling's entry.
    #[tokio::test]
    async fn a_nest_claiming_a_blessed_siblings_id_renews_nothing() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder()];
        let (machine, store, plat) = trust_machine(nest.clone(), &kp);
        let home = home_id(&machine).await;
        machine
            .dispatch(LinkedNestsAction::Mint {
                nest_id: home.clone(),
                holder_bridge_id: "mda-1".into(),
                scope: vec![label_write_scope()],
                duration: Some(TrustGrantDuration::Standard),
            })
            .await
            .unwrap();
        // The user blessed a sibling nest; this one they never blessed.
        let sibling = to_hex(&[0x02u8; 32]);
        machine
            .dispatch(LinkedNestsAction::SetBlessed {
                nest_id: sibling.clone(),
                blessed: true,
            })
            .await
            .unwrap();
        // The connected nest now claims the sibling's id.
        *nest.claimed_id.lock().unwrap() = Some(vec![0x02u8; 32]);

        plat.set_now(NOW + 80 * DAY);
        machine
            .dispatch(LinkedNestsAction::AutoRenew)
            .await
            .unwrap();
        machine.refresh().await.unwrap();

        assert!(
            nest.state.lock().unwrap().renewed.is_empty(),
            "no renewal on the strength of a claimed id"
        );
        assert!(
            stored_events(&store)
                .iter()
                .all(|e| e.kind != GrantEventKind::Renew),
            "and no Renew logged"
        );
        let row = machine.snapshot().home.unwrap();
        assert_eq!(row.nest_id, home, "the row names the proven identity");
        assert!(!row.blessed);
        assert!(
            row.trust_grants
                .iter()
                .all(|g| g.liveness != TrustLiveness::AutoRenewing)
        );
    }

    /// The `(epoch, factor)` of every wrap a fake-nest `renew_grant` call
    /// carried, decoded from the canonical bytes the machine sent.
    fn renewed_wrap_epochs(keys: &[Vec<u8>]) -> Vec<(Option<u64>, Option<String>)> {
        keys.iter()
            .map(|b| {
                let w = fauna_mls::wrapped_blob::WrappedScopeKey::from_canonical_bytes(b)
                    .expect("a canonical WrappedScopeKey");
                (w.epoch, w.scope.factor.clone())
            })
            .collect()
    }

    /// A bounded mail grant's renewal carries its epoch wraps — the tick and
    /// the manual renew both compute them from the log's recorded end, one
    /// per sealing epoch the extension newly covers, sealed to the holder the
    /// live roster names and each confined to the grant's own labeler — so a
    /// subscribed labeler's trust no longer lapses unrenewable at its window's
    /// end (`nests.md` § Expiry / renewal → *Duration and blessing*, the
    /// sentence that lifts both refusals).
    #[tokio::test]
    async fn a_bounded_mail_grant_renews_with_its_epoch_wraps() {
        use fauna_mls::wrapped_blob::mail_sealing_epoch_of;
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder()];
        let mut cfg = trust_cfg(kp.actor_id());
        let labeler = fauna_core::identity::ActorId([0x1Du8; 32]);
        let factor = fauna_core::scoring::labeler_factor(&labeler);
        let grant_id = [0x42u8; 16];
        let minted_end = NOW + 90 * DAY;
        record_mint_cfg(
            &mut cfg,
            kp.signing_key(),
            grant_id,
            mda_holder().pubkey,
            grant_log::bounded_mail_labeler_event_scope(&labeler),
            NOW,
            minted_end,
            NOW,
        )
        .unwrap();
        let (machine, store, plat) =
            trust_machine_traced(nest.clone(), &kp, cfg, mail_enabled(), None);
        let home = home_id(&machine).await;
        machine
            .dispatch(LinkedNestsAction::SetBlessed {
                nest_id: home,
                blessed: true,
            })
            .await
            .unwrap();

        // The tick: 80 days in the grant is due, and the renewal to now + 90 d
        // crosses ~11 weekly epochs past the recorded end.
        plat.set_now(NOW + 80 * DAY);
        machine
            .dispatch(LinkedNestsAction::AutoRenew)
            .await
            .unwrap();
        let renewed = nest.state.lock().unwrap().renewed.clone();
        assert_eq!(renewed.len(), 1, "the tick renews a bounded grant too");
        let (id, tick_start, tick_end, keys) = &renewed[0];
        assert_eq!(*id, grant_id);
        assert_eq!(*tick_end, NOW + 80 * DAY + 90 * DAY, "now + its own length");
        assert_eq!(
            *tick_start, NOW,
            "now - its own length is behind the mint start: the start stays"
        );
        let expected: Vec<_> = (mail_sealing_epoch_of(minted_end) + 1
            ..=mail_sealing_epoch_of(*tick_end))
            .map(|e| (Some(e), Some(factor.clone())))
            .collect();
        assert!(expected.len() > 1, "the extension spans epochs");
        assert_eq!(
            renewed_wrap_epochs(keys),
            expected,
            "exactly the extension epochs' wraps, each confined to the labeler"
        );
        assert!(
            stored_events(&store)
                .iter()
                .any(|e| e.kind == GrantEventKind::Renew),
            "History records the tick's renew"
        );
        let row = machine.snapshot().home.unwrap();
        assert_eq!(
            row.trust_grants[0].liveness,
            TrustLiveness::AutoRenewing,
            "and the page says so"
        );

        // The manual renew: from the end the tick moved to, the next
        // extension's epochs.
        plat.set_now(NOW + 100 * DAY);
        machine
            .dispatch(LinkedNestsAction::Renew {
                grant_id: grant_id.to_vec(),
            })
            .await
            .expect("the manual renew no longer refuses a bounded grant");
        let renewed = nest.state.lock().unwrap().renewed.clone();
        assert_eq!(renewed.len(), 2);
        let (_, manual_start, manual_end, keys) = &renewed[1];
        assert_eq!(*manual_end, NOW + 100 * DAY + 90 * DAY);
        assert_eq!(
            *manual_start,
            NOW + 10 * DAY,
            "the window re-centres: one length behind the renewal instant"
        );
        let expected: Vec<_> = (mail_sealing_epoch_of(*tick_end) + 1
            ..=mail_sealing_epoch_of(*manual_end))
            .map(|e| (Some(e), Some(factor.clone())))
            .collect();
        assert!(!expected.is_empty());
        assert_eq!(renewed_wrap_epochs(keys), expected, "from the recorded end");
    }

    /// The wraps are sealed to the holder the LIVE roster names, never to key
    /// material the log would have to store: a bounded grant whose holder the
    /// roster no longer lists gets no keyless bump — the manual renew refuses
    /// with the recorded end unmoved (a bump would move it and skip the epochs
    /// in between at the next keyed renewal).
    #[tokio::test]
    async fn a_bounded_mail_grant_whose_holder_left_the_roster_is_not_renewed_keyless() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        // The roster lists another holder; the grant's `mda` holder is gone.
        nest.state.lock().unwrap().holders = vec![web_serve_holder()];
        let mut cfg = trust_cfg(kp.actor_id());
        let grant_id = [0x42u8; 16];
        record_mint_cfg(
            &mut cfg,
            kp.signing_key(),
            grant_id,
            mda_holder().pubkey,
            vec![grant_log::bounded_mail_event_scope()],
            NOW,
            NOW + 90 * DAY,
            NOW,
        )
        .unwrap();
        let (machine, store, plat) =
            trust_machine_traced(nest.clone(), &kp, cfg, mail_enabled(), None);

        plat.set_now(NOW + 80 * DAY);
        let manual = machine
            .dispatch(LinkedNestsAction::Renew {
                grant_id: grant_id.to_vec(),
            })
            .await;
        assert!(manual.is_err(), "the manual renew refuses");
        assert!(
            nest.state.lock().unwrap().renewed.is_empty(),
            "no keyless bump reached the nest"
        );
        assert!(
            stored_events(&store)
                .iter()
                .all(|e| e.kind != GrantEventKind::Renew),
            "no Renew moved the recorded end"
        );
    }

    #[tokio::test]
    async fn set_lens_flips_home_row_without_nest_call() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder()];
        let (machine, _store, _plat) = trust_machine(nest.clone(), &kp);
        machine.hydrate().await.unwrap();
        let home_id = machine.snapshot().home.unwrap().nest_id;

        machine
            .dispatch(LinkedNestsAction::SetLens {
                nest_id: home_id,
                lens: TrustLens::History,
            })
            .await
            .unwrap();

        assert_eq!(machine.snapshot().home.unwrap().lens, TrustLens::History);
    }

    #[tokio::test]
    async fn set_lens_survives_a_refresh() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default());
        nest.state.lock().unwrap().holders = vec![mda_holder()];
        let (machine, _store, _plat) = trust_machine(nest.clone(), &kp);
        machine.hydrate().await.unwrap();
        let home_id = machine.snapshot().home.unwrap().nest_id;
        machine
            .dispatch(LinkedNestsAction::SetLens {
                nest_id: home_id,
                lens: TrustLens::History,
            })
            .await
            .unwrap();

        machine.dispatch(LinkedNestsAction::Refresh).await.unwrap();

        assert_eq!(
            machine.snapshot().home.unwrap().lens,
            TrustLens::History,
            "lens is local UI state, preserved across a nest refresh"
        );
    }

    #[tokio::test]
    async fn hydrate_shows_home_with_empty_facet_when_no_holders() {
        let kp = ActorKeypair::generate();
        let nest = Arc::new(FakeNest::default()); // no holders
        let (machine, _store, _plat) = trust_machine(nest.clone(), &kp);

        machine.hydrate().await.unwrap();

        let home = machine
            .snapshot()
            .home
            .expect("home row present even with no holders");
        assert!(home.is_home);
        assert!(home.trust_grants.is_empty(), "nest-trust-empty state");
    }

    #[tokio::test]
    async fn pairing_only_machine_has_no_home_and_rejects_trust_actions() {
        let nest = Arc::new(FakeNest::default());
        let machine = LinkedNestsMachine::new(nest as Arc<dyn LinkedNestsNest>);
        machine.hydrate().await.unwrap();
        assert!(
            machine.snapshot().home.is_none(),
            "no home row without trust seams"
        );

        let err = machine
            .dispatch(LinkedNestsAction::Mint {
                nest_id: "x".into(),
                holder_bridge_id: "mda-1".into(),
                scope: vec![],
                duration: Some(TrustGrantDuration::Standard),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, PairDispatchError::InvalidState(_)));
    }

    // ── Wire-contract tests (PairClient<R: RpcRequester>) ───────────────────
    //
    // The `LinkedNestsMachine` tests above drive a `FakeNest` seam, so they
    // never exercise the generic `PairClient<R>` adapter that composes the
    // `fauna.pair.*` kind strings and typed payloads sent on the wire. These
    // pin both: each `PairClient` method must send its exact kind and a payload
    // that round-trips back to the typed request. A nest-side kind rename or a
    // request-shape drift would break them silently otherwise (the native/wasm
    // seams below just forward through `PairClient`, with no other Rust-layer
    // guard). The pattern mirrors the `RecordingRequester` in
    // `fauna-client-conversations` / `-events` / `-snapshots` / `-sync`
    // (transport-free single-poll, so it runs on every target including wasm);
    // real end-to-end round-trip conformance lives in the nest-side pairing
    // suites.

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        // Answer with a reply the requested `Reply` type decodes — one arm
        // per kind, each the minimal valid shape.
        match kind {
            "fauna.pair.list" => fauna_protocol::encode_canonical(&PairListReply {
                pairings: vec![],
                forward_queue: Default::default(),
                extra: Default::default(),
            }),
            "fauna.pair.forward_retry" => {
                fauna_protocol::encode_canonical(&PairForwardRetryReply {
                    rearmed: 0,
                    extra: Default::default(),
                })
            }
            "fauna.pair.forward_discard" => {
                fauna_protocol::encode_canonical(&PairForwardDiscardReply {
                    discarded: 0,
                    extra: Default::default(),
                })
            }
            "fauna.pair.add" => fauna_protocol::encode_canonical(&PairAddReply {
                ok: true,
                extra: Default::default(),
            }),
            "fauna.pair.revoke" => fauna_protocol::encode_canonical(&PairRevokeReply {
                ok: true,
                extra: Default::default(),
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    fn pair_client() -> (Arc<RecordingRequester>, PairClient<Arc<RecordingRequester>>) {
        let rec = Arc::new(RecordingRequester::new(reply));
        let client = PairClient::new(rec.clone());
        (rec, client)
    }

    #[test]
    fn list_composes_kind_and_payload() {
        let (rec, c) = pair_client();
        block_on(c.list()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.pair.list");
        let _req: PairListRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn forward_retry_and_discard_compose_their_kinds() {
        let (rec, c) = pair_client();
        block_on(c.forward_retry()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.pair.forward_retry");
        let _req: PairForwardRetryRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");

        let (rec, c) = pair_client();
        block_on(c.forward_discard()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.pair.forward_discard");
        let _req: PairForwardDiscardRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn add_composes_kind_and_payload() {
        let (rec, c) = pair_client();
        block_on(c.add(PairAddRequest {
            private_nest_id: ByteBuf::from(vec![0xab; 32]),
            capabilities: vec!["mls_pull".into()],
            expires_at: Some(1_900_000_000),
            label: Some("home NAS".into()),
            nest_url: Some("https://nas.example".into()),
            extra: Default::default(),
        }))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.pair.add");
        let req: PairAddRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.private_nest_id.as_ref(), vec![0xab; 32].as_slice());
        assert_eq!(req.capabilities, vec!["mls_pull".to_string()]);
        assert_eq!(req.label.as_deref(), Some("home NAS"));
    }

    #[test]
    fn revoke_composes_kind_and_payload() {
        let (rec, c) = pair_client();
        block_on(c.revoke(vec![0xcd; 32])).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.pair.revoke");
        let req: PairRevokeRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.private_nest_id.as_ref(), vec![0xcd; 32].as_slice());
    }
}
