//! The page-level ATProto login-plane settings machine.
//!
//! Mirrors `fauna_labeler_catalog_machine::LabelerCatalogMachine`: each app
//! holds an `Arc<AtprotoSettingsMachine>`, observes via a registered
//! [`AtprotoSettingsObserver`], drives gestures, and renders the whole
//! app-credentials + connected-apps surface off `snapshot()`.
//!
//! Uses `std::sync::Mutex` (not tokio's) so getters and sync gestures work from
//! any thread context — including UI threads and `#[tokio::test]`. The async
//! gestures snapshot/clone under the lock, drop it, do IO, then re-acquire; the
//! lock is never held across an `await`.
//!
//! ## The custody split this machine implements (D3)
//!
//! A credential's **secret** is generated here, on the client, and persisted
//! only to the account plane's `fauna.state.atproto` — one sealed row per
//! credential, synced across the user's own devices, reached through the
//! injected [`AtprotoCredentialStore`]. The nest receives only the Argon2id
//! **PHC verifier** and can never recover the secret — not at mint, not later.
//! So:
//!
//! * the nest is authoritative for *which credentials exist* (the list, and
//!   revocation);
//! * the account's credential store is authoritative for *what the secret is*
//!   (mint's one-time return, and re-reveal);
//! * the two are joined only here, into
//!   [`AppCredentialRow::revealable`](crate::snapshots::AppCredentialRow::revealable).
//!
//! ## Write ordering — nest first, local persist second
//!
//! Every mutation calls the nest **before** touching the credential store, matching the
//! shipped mail precedent (`fauna_client_mail_settings`'s `add_credential` /
//! `revoke_credential`). The ordering is a crash-safety choice, not a stylistic
//! one — the project invariant is that every state a client can reach must be
//! recoverable *by a client*, with no server-side surgery
//! (`docs/goal/architecture/nest/common.md` § Client-state recoverability):
//!
//! * **nest-then-local**, interrupted in between ⇒ a live nest row whose secret
//!   this device never stored. Visible in the list, `revealable: false`,
//!   recoverable by revoke + re-mint from any client. Degraded, never stuck.
//! * **local-then-nest**, interrupted in between ⇒ a secret in the `fauna.state.atproto` custody for
//!   a credential the nest has never heard of. It renders nowhere (the list is
//!   nest-derived), so no client affordance can reach it: silent orphaned
//!   secret material that only accretes. Strictly worse.
//!
//! The same reasoning makes [`Self::mint`] hand back the secret **even when the
//! local persist fails** — see its doc comment.

use std::sync::{Arc, Mutex};

use fauna_core::control_chars::strip_control_chars;
use fauna_core::data::{AtprotoAppCredential, Timestamp};
use fauna_core::identity::ActorKeypair;
use fauna_core::localized::LocalizedText;
use fauna_core::secret::{SecretByteBuf, SecretString};
use fauna_protocol::atproto::{IntegrationLevel, LevelContext, TransitionPlan};

use fauna_client_alerts::CriticalAlerts;
use fauna_client_atproto::identity_store::{AtprotoIdentityStore, NoAccountRuntime};
use fauna_client_atproto::rotation_key::{
    mint_rotation_key, record_contest_intent, record_nest_named_did, record_published_binding,
    record_tombstone_consent,
};
use fauna_client_bridges::atproto_credential::{
    compute_app_credential_verifier, generate_app_credential,
};
use fauna_client_bridges::atproto_delegation::{
    AUTHORING_CAPABILITIES, DELEGATION_WINDOW_SECS, build_authoring_delegation_cert,
    delegation_liveness, parse_delegation_cert,
};
use fauna_client_bridges::derive_credential_id;
use fauna_client_config::StoreError;

use crate::credentials::AtprotoCredentialStore;

use crate::consent_grant::{self, AnswerError, ConsentGrantSeams};
use crate::contest::{
    ContestActor, ContestEligibility, ContestPlan, ContestProgress, DirectoryContestActor,
    Violation,
};
use crate::custody::{GenesisVerifier, SeniorityVerdict};
use crate::error::AtprotoSettingsError;
use crate::nest_api::{
    AtprotoSettingsNestApi, LinkSummary, NestConsentRow, NestConsentSet, NestIdentitySummary,
};
use crate::observer::AtprotoSettingsObserver;
use crate::retirement::{DirectoryTombstoneActor, RetirementProgress, TombstoneActor};
use crate::snapshots::{
    AppCredentialRow, AtprotoGrantRow, AtprotoSessionRow, AtprotoSettingsSnapshot, ConsentCardRow,
    ConsentSetRow, ContestCardRow, ContestConfirmCardModel, DelegationRow, DeleteConfirmCardModel,
    IdentitySummaryRow, LinkSummaryRow, RetireIdentityOptIn, TransitionCardModel,
};
use std::collections::HashMap;

/// i18n key for a page read (`refresh()`) failure.
const REFRESH_ERROR_KEY: &str = "atproto_settings.error_refresh";
/// i18n key for a `mint` failure.
const MINT_ERROR_KEY: &str = "atproto_settings.error_mint";
/// i18n key for a `revoke` failure.
const REVOKE_ERROR_KEY: &str = "atproto_settings.error_revoke";
/// i18n key for a `revoke_session` failure.
const REVOKE_SESSION_ERROR_KEY: &str = "atproto_settings.error_revoke_session";
/// i18n key for a kill-switch toggle failure.
const TOGGLE_ERROR_KEY: &str = "atproto_settings.error_toggle";
/// i18n key for an authorize-external-apps (delegation mint) failure.
const AUTHORIZE_ERROR_KEY: &str = "atproto_settings.error_authorize";
/// i18n key for a delegation-revoke failure.
const DEAUTHORIZE_ERROR_KEY: &str = "atproto_settings.error_deauthorize";
/// i18n key for "this app cannot authorize external apps because it has no
/// identity key wired" — a shell-capability gap, not a nest failure.
const NO_IDENTITY_ERROR_KEY: &str = "atproto_settings.error_no_identity";
/// i18n key for "the nest served a delegation cert this account did not sign".
/// A custody-grade mismatch, surfaced rather than rendered as a normal row.
const DELEGATION_UNTRUSTED_KEY: &str = "atproto_settings.error_delegation_untrusted";
/// i18n key for "the credential is live on the nest, but this device could not
/// store its secret" — the mint's partial-success case.
const SAVE_LOCAL_ERROR_KEY: &str = "atproto_settings.error_save_local";

/// The "also permanently retire this identity" opt-in failing to record.
const REQUEST_TOMBSTONE_ERROR_KEY: &str = "atproto_settings.error_request_tombstone";
const REQUEST_CONTEST_ERROR_KEY: &str = "atproto_settings.error_request_contest";
/// The OAuth session plane's wire spelling — the nest's own
/// (`bins/fauna-nest/src/bridge_atproto_handlers.rs`'s `VALID_PLANES`). A
/// suspended row has no session row to copy it from, so it is named here once
/// rather than spelled inline at the one site that synthesizes such a row.
const OAUTH_PLANE: &str = "oauth";
/// i18n key for a `confirm_transition` failure (the card stays open).
const TRANSITION_ERROR_KEY: &str = "atproto_settings.error_transition";
/// i18n key for a `resolve_consent` failure — the call itself did not land.
const CONSENT_ERROR_KEY: &str = "atproto_settings.error_consent";
/// i18n key for "there was no live request to answer". Deliberately ONE string
/// for all three causes (answered elsewhere / expired / not yours), because nest
/// decides them in one `WHERE` clause and reports one `resolved: false`.
const CONSENT_GONE_KEY: &str = "atproto_settings.error_consent_gone";
/// i18n key for the greyed hosted options' reason line (arg: the domain).
const GATE_REASON_KEY: &str = "atproto_settings.gate_reason";
// The contest ceremony's confirm-card copy (`atproto-contest-confirm-card`),
// composed by `compose_contest_confirm_lines` below.
const CONTEST_CONFIRM_UNDO_KEY: &str = "atproto_settings.contest_confirm_undo";
const CONTEST_CONFIRM_SIGNS_KEY: &str = "atproto_settings.contest_confirm_signs";
const CONTEST_CONFIRM_DIRECTORY_KEY: &str = "atproto_settings.contest_confirm_directory_rules";
/// i18n key for "this violation cannot be fought, so there is nothing to
/// confirm" — the loud refusal a page should never be able to provoke, since
/// `atproto-contest` renders only at `show_contest`.
const CONTEST_NOT_CONTESTABLE_KEY: &str = "atproto_settings.error_contest_not_contestable";
// The delete ceremony's confirm-card copy (`atproto-delete-confirm-card`),
// composed by `compose_delete_confirm_lines` below. The honest caveat is
// deliberately NOT a fourth key here: it is the transition card's own
// `CARD_NO_RECALL_KEY`, reused rather than re-worded, for the reason the
// contest card reuses its deadline line — one promise worded twice is two
// chances to disagree on the screen where a user is deciding.
const DELETE_CONFIRM_SWEEP_KEY: &str = "atproto_settings.delete_confirm_sweep";
const DELETE_CONFIRM_IDENTITY_KEPT_KEY: &str = "atproto_settings.delete_confirm_identity_kept";
const DELETE_CONFIRM_APPS_KEY: &str = "atproto_settings.delete_confirm_apps_disconnected";
const DELETE_CONFIRM_LEVEL_OFF_KEY: &str = "atproto_settings.delete_confirm_level_off";
/// The terminal line that REPLACES `DELETE_CONFIRM_IDENTITY_KEPT_KEY` while the
/// retire opt-in is ticked (arg: the handle) — the two are mutually exclusive.
const DELETE_CONFIRM_IDENTITY_RETIRED_KEY: &str =
    "atproto_settings.delete_confirm_identity_retired";
/// The retire opt-in's greyed reasons: did:web has no operation log to
/// tombstone, and a did:plc not yet published has nothing in the directory.
const DELETE_RETIRE_UNAVAILABLE_WEB_KEY: &str = "atproto_settings.delete_retire_unavailable_web";
const DELETE_RETIRE_UNAVAILABLE_UNPUBLISHED_KEY: &str =
    "atproto_settings.delete_retire_unavailable_unpublished";
/// i18n key for a `delete_presence` failure (the card stays open; retry safe).
const DELETE_PRESENCE_ERROR_KEY: &str = "atproto_settings.error_delete_presence";
/// i18n key for "there is no presence left to delete" — the loud refusal a page
/// should never be able to provoke, since `atproto-delete-presence` renders
/// only at `show_delete_presence`.
const DELETE_NOTHING_TO_DELETE_KEY: &str = "atproto_settings.error_nothing_to_delete";
// The transition card's copy keys — one line per `TransitionPlan` effect,
// composed by `compose_card_lines` below. The single-source rule
// (`ui/atproto.md` § Transition semantics): nest executes the plan, this card
// describes it; clients render these lines verbatim and never build their own
// transition table.
const CARD_UNLINK_KEY: &str = "atproto_settings.card_unlink";
const CARD_MINT_KEY: &str = "atproto_settings.card_mint";
const CARD_REACTIVATE_KEY: &str = "atproto_settings.card_reactivate";
const CARD_PUBLISH_CONSENT_KEY: &str = "atproto_settings.card_publish_consent";
const CARD_DEACTIVATE_KEY: &str = "atproto_settings.card_deactivate";
const CARD_NO_RECALL_KEY: &str = "atproto_settings.card_no_recall";
const CARD_DELETE_POINTER_KEY: &str = "atproto_settings.card_delete_pointer";
const CARD_OPEN_PLANE_KEY: &str = "atproto_settings.card_open_plane";
const CARD_DM_HONESTY_KEY: &str = "atproto_settings.card_dm_honesty";
const CARD_SUSPEND_PLANE_KEY: &str = "atproto_settings.card_suspend_plane";

/// Internal page state. In-memory only; clients read snapshots via the getter.
struct State {
    // ── depth selector ──────────────────────────────────────────────────
    level: IntegrationLevel,
    hosted_allowed: bool,
    handle_domain: String,
    handle_preview: String,
    identity: Option<IdentitySummaryRow>,
    /// The verified D10 delegation row, or `None` when none is provisioned.
    delegation: Option<DelegationRow>,
    link: Option<LinkSummaryRow>,
    pending: Option<PendingTransition>,
    did_method: String,
    history_backfill: bool,
    // ── S4-C custody check ──────────────────────────────────────────────
    /// The minted DID as the last status read reported it (never rendered).
    custody_did: Option<String>,
    /// The S5 slice-5b opt-in as the last status read reported it: this
    /// identity is to be permanently retired and no tombstone has been
    /// published yet. Never rendered — the page shows the *presence* is
    /// deleted; this is the background act the converge pass still owes.
    tombstone_requested: bool,
    /// (did, held-ring fingerprint) pairs already verified this session — a
    /// passing verdict is cached; mismatches and failures are re-checked
    /// every refresh so a fixed directory clears the alarm. Keyed on the
    /// whole ring, so a keyring change (a fresh re-mint key syncing in)
    /// re-verifies rather than trusting a stale pass.
    custody_ok: std::collections::HashSet<(String, String)>,
    /// A custody contradiction seen ONCE, awaiting confirmation on the next
    /// convergence before it alarms — `(did, mismatch fingerprint)`. The
    /// debounce exists for one legitimate race: a sibling device re-mints
    /// with a fresh senior key (fresh-key-per-mint) and this device reads
    /// the new DID's log before its custody rows have synced the new
    /// key — a Mismatch that the plane sync resolves. A real attack persists
    /// into the next convergence (PLC's 72 h contest window dwarfs one
    /// pass), so confirming costs nothing material; a critical-alert flash
    /// that self-clears would cost the alarm its credibility.
    custody_suspect: Option<(String, String)>,
    /// The DID whose custody mismatch has been CONFIRMED (survived the
    /// debounce), i.e. the alarm is standing. `None` whenever custody holds.
    ///
    /// This is the recovery-fork pass's trigger, and the gate is not an
    /// optimization detail: the contest is the remedy for an alarm, so with no
    /// alarm there is nothing to plan for — and planning anyway would make
    /// every settings refresh of every healthy identity issue a second HTTPS
    /// request to the public PLC directory on top of the custody check's own.
    /// Kept beside the alarm rather than read back from the alerts registry
    /// because that registry is `None` on shells with no banner yet, and the
    /// remedy must not depend on whether a shell can draw the warning.
    custody_alarmed: Option<String>,
    /// The contest card the last convergence composed, plus the CID it is
    /// scoped to. `None` in every ordinary state.
    ///
    /// The CID is held HERE rather than handed to clients on the snapshot:
    /// [`AtprotoSettingsMachine::request_contest`] takes no arguments and
    /// records consent for exactly the op the card just described, so no
    /// client — and no compromised view layer — can redirect the gesture at a
    /// different operation (decision 6's scoping, enforced by construction).
    contest: Option<(String, ContestCardRow)>,
    /// The ceremony's confirm card is open, and whether its submit is in
    /// flight. `None` whenever the user has not opened it.
    ///
    /// Kept as ceremony state on the MACHINE rather than as a `bool` in each of
    /// seven pages: this is the last surface before a signature that rewrites
    /// who controls an identity, so "may it open at all" and "is the button
    /// still live" are decided once, here. It is never persisted — a restart
    /// mid-ceremony is a closed card over a violation that still stands, which
    /// is the honest state.
    contest_confirm: Option<ContestConfirm>,
    /// The "Delete my Bluesky presence" ceremony, open only on the user's
    /// explicit gesture. Machine-side for the contest ceremony's reason: this
    /// is the last surface before the one destructive call on this page, so
    /// "may it open at all" and "is the confirm still live" are decided once,
    /// here, rather than by seven pages each remembering. Never persisted — a
    /// restart mid-ceremony is a closed card over a presence that still
    /// stands, which is the honest state.
    delete_confirm: Option<DeleteConfirm>,
    // ── F1 login plane ──────────────────────────────────────────────────
    credentials: Vec<AppCredentialRow>,
    sessions: Vec<AtprotoSessionRow>,
    external_apps_enabled: bool,
    // ── F4 rung 2: the OAuth consent ceremony ───────────────────────────
    /// Live consent requests this account may answer, oldest first. Almost
    /// always empty — a row exists only while an external app is mid-ceremony.
    consents: Vec<ConsentCardRow>,
    /// The raw rows behind [`Self::consents`], same order — what an approve
    /// reads the attested keys and the manifest from (`crate::consent_grant`).
    consent_rows: Vec<NestConsentRow>,
    error: Option<LocalizedText>,
}

/// A staged (not yet confirmed) level change: the plan the card describes and
/// the confirm executes.
struct PendingTransition {
    target: IntegrationLevel,
    plan: TransitionPlan,
    in_progress: bool,
}

/// What one reporting converge pass did, for the gesture that asked for it.
/// Only ever produced when the caller is a user press — the ambient pass
/// discards every outcome by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
enum ContestOutcome {
    /// The directory accepted the fork.
    Contested,
    /// Nothing was signed, or the submit was refused. Carries the detail for
    /// `error_request_contest`'s `{message}`.
    Refused(String),
}

/// The contest ceremony's open confirm card. Carries no copy of the violation:
/// the card is composed at snapshot time off `State::contest`, so a ceremony
/// can never describe an op that stopped standing.
struct ContestConfirm {
    in_progress: bool,
}

/// The delete ceremony's open confirm card. Carries no copy of the presence:
/// the card is composed at snapshot time off `State::identity`, so a ceremony
/// can never describe an identity that stopped standing — and a convergence
/// that takes the presence down takes the card with it without having to
/// remember to (the `contest_confirm` derivation, same reason).
struct DeleteConfirm {
    in_progress: bool,
    /// The `atproto-delete-tombstone` tick, scoped to THIS opening of the card
    /// — a fresh opening starts unticked, so the terminal act can never be
    /// carried over from an earlier, abandoned ceremony.
    retire_identity: bool,
    /// The sweep of this ceremony already landed but its ticked retirement
    /// step did not. Keeps the card open past the presence it swept, because
    /// the button that opens it withdraws once the presence is deleted — so
    /// closing it here would strand the user's request with no way to finish
    /// it. A retry re-sends the idempotent sweep and then the retirement.
    swept: bool,
}

impl State {
    fn new() -> Self {
        Self {
            // Level defaults to the nest column default (`off`) before the
            // first fetch — same convention as the kill-switch below. The
            // hosted rungs start greyed (`hosted_allowed: false`) until a
            // fetch proves the gate passes: a first paint must not offer a
            // hosted entry the nest would grey.
            level: IntegrationLevel::Off,
            hosted_allowed: false,
            handle_domain: String::new(),
            handle_preview: String::new(),
            identity: None,
            delegation: None,
            link: None,
            pending: None,
            did_method: "plc".to_string(),
            history_backfill: false,
            custody_did: None,
            tombstone_requested: false,
            custody_ok: std::collections::HashSet::new(),
            custody_suspect: None,
            custody_alarmed: None,
            contest: None,
            contest_confirm: None,
            delete_confirm: None,
            credentials: Vec::new(),
            sessions: Vec::new(),
            // Default ON, matching the nest's `atproto_account_settings`
            // column default: a first paint before any fetch must not render
            // the kill-switch as engaged when it is not.
            external_apps_enabled: true,
            consents: Vec::new(),
            consent_rows: Vec::new(),
            error: None,
        }
    }

    fn level_context(&self) -> LevelContext {
        LevelContext {
            has_external_link: self.link.is_some(),
            // A retired identity is not a restorable one — the same reading
            // nest applies, so the card and the execution agree on whether
            // re-entry restores a DID or mints a fresh one. Reading it any
            // other way here would promise the user a restoration that nest
            // would then not perform.
            has_identity: self
                .identity
                .as_ref()
                .is_some_and(|i| i.status != "tombstoned"),
        }
    }
}

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct AtprotoSettingsMachine {
    state: Mutex<State>,
    observer: Arc<dyn AtprotoSettingsObserver>,
    nest_api: Arc<dyn AtprotoSettingsNestApi>,
    /// The ATProto identity custody — the held senior rotation keys, the
    /// tombstone consents, the contest intents, the nest-named DIDs — on the
    /// account plane (`fauna.state.atproto-identity`, born plane-only).
    /// Wired post-construction by [`Self::set_identity_store`], like the
    /// devices machine's fleet door: every host hands in
    /// `fauna_account_seams::atproto_identity::RuntimeAtprotoIdentity` over
    /// its runtime handle (web's settings chunk, the account port's
    /// forwarder). Until then [`NoAccountRuntime`] refuses every read and
    /// write — the custody checks read that as "cannot verify", quiet.
    identity_store: Mutex<Arc<dyn AtprotoIdentityStore>>,
    /// Where the minted credentials' secrets rest — the account plane's
    /// `fauna.state.atproto` ([`AtprotoCredentialStore`]), wired
    /// post-construction by the host ([`Self::set_credential_store`]): the
    /// native seats over their account-store handle, web's ATProto chunk over
    /// the account port. `None` refuses every credential read and write.
    credentials: Mutex<Option<Arc<dyn AtprotoCredentialStore>>>,
    /// The owner-side seams the consent-time grant to a third-party app needs
    /// (`crate::consent_grant`), wired post-construction by the host
    /// ([`Self::set_consent_grant_seams`]). `None` refuses an approve naming
    /// a `fauna:records:` scope rather than resolving it keyless.
    consent_grants: Mutex<Option<Arc<ConsentGrantSeams>>>,
    /// The S4-C genesis-seniority check (production: the direct PLC-directory
    /// read; tests: programmed verdicts).
    verifier: Arc<dyn GenesisVerifier>,
    /// The S5 slice-5b terminal retirement (production: the client-direct
    /// sweep probe + PLC tombstone submit; tests: programmed answers). Runs on
    /// the same status convergence as the custody check.
    retirement: Arc<dyn TombstoneActor>,
    /// The 72 h recovery-fork contest (production: the client-direct PLC
    /// directory read + fork submit; tests: a modelled log). Runs on the same
    /// status convergence as the custody check that finds the violation it
    /// answers.
    contest: Arc<dyn ContestActor>,
    /// The app-wide critical-alerts registry the custody alarm posts to.
    /// `None` on platforms whose shell doesn't render the banner yet (the
    /// check still runs and logs; `critical-alerts.md` § Implementation
    /// status).
    alerts: Option<Arc<CriticalAlerts>>,
    /// The account's identity keypair — held **only** to mint the D10
    /// authoring-delegation cert, which is identity-signed by definition
    /// (`atproto-pds-full.md` § D10: "the identity key stays client-only", so
    /// a nest that could mint its own authorization would be self-granting).
    ///
    /// `None` on a shell that has not wired it, which degrades honestly: the
    /// delegation *status* row still renders and revoke still works, only
    /// authorizing is refused with a page error. It is deliberately not on the
    /// nest seam — a seam implementation must not be able to receive a signing
    /// key — and it never reaches a snapshot.
    identity: Option<ActorKeypair>,
}

impl AtprotoSettingsMachine {
    /// Construct the page machine over an injected [`AtprotoSettingsNestApi`];
    /// the account-plane seams (identity custody, credential store) are wired
    /// after construction. State starts empty; the client calls `refresh()` to
    /// populate it.
    ///
    /// Not a `#[uniffi::constructor]` — the seams (`Arc<dyn …>`) have no FFI
    /// ABI. Clients construct via `nest_api::build_atproto_settings_machine`
    /// (native `fauna-ffi` / linux, wasm web); tests pass the fakes.
    pub fn new(
        observer: Arc<dyn AtprotoSettingsObserver>,
        nest_api: Arc<dyn AtprotoSettingsNestApi>,
        verifier: Arc<dyn GenesisVerifier>,
        alerts: Option<Arc<CriticalAlerts>>,
    ) -> Arc<Self> {
        Self::new_with_identity(observer, nest_api, verifier, alerts, None)
    }

    /// [`Self::new`] plus the identity keypair the D10 delegation mint signs
    /// with. Separate constructor so existing call sites keep compiling and a
    /// shell opts into the authoring half explicitly.
    pub fn new_with_identity(
        observer: Arc<dyn AtprotoSettingsObserver>,
        nest_api: Arc<dyn AtprotoSettingsNestApi>,
        verifier: Arc<dyn GenesisVerifier>,
        alerts: Option<Arc<CriticalAlerts>>,
        identity: Option<ActorKeypair>,
    ) -> Arc<Self> {
        Self::new_with_seams(
            observer,
            nest_api,
            verifier,
            alerts,
            identity,
            Arc::new(DirectoryTombstoneActor),
            Arc::new(DirectoryContestActor),
        )
    }

    /// [`Self::new_with_identity`] plus the two identity-custody seams
    /// (terminal retirement, recovery-fork contest). The production seams are
    /// the only ones clients ever want, so the two constructors above default
    /// them; this entry exists for tests, which drive both converge passes'
    /// branches without touching the network.
    #[allow(clippy::too_many_arguments)] // Every argument is a distinct
    // injected seam; bundling them into a struct would just move the same list
    // one level out while making the two production constructors above longer.
    pub fn new_with_seams(
        observer: Arc<dyn AtprotoSettingsObserver>,
        nest_api: Arc<dyn AtprotoSettingsNestApi>,
        verifier: Arc<dyn GenesisVerifier>,
        alerts: Option<Arc<CriticalAlerts>>,
        identity: Option<ActorKeypair>,
        retirement: Arc<dyn TombstoneActor>,
        contest: Arc<dyn ContestActor>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::new()),
            observer,
            nest_api,
            identity_store: Mutex::new(Arc::new(NoAccountRuntime)),
            credentials: Mutex::new(None),
            consent_grants: Mutex::new(None),
            verifier,
            retirement,
            contest,
            alerts,
            identity,
        })
    }

    /// Wire the account plane's ATProto identity custody door — every host
    /// calls this right after construction, before the first `refresh()`
    /// (the devices machine's `set_fleet_removal` shape). Natively the
    /// seat's `RuntimeAtprotoIdentity`; on web the account port's forwarder
    /// (`fauna_client_atproto::port::PortAtprotoIdentityStore`).
    pub fn set_identity_store(&self, store: Arc<dyn AtprotoIdentityStore>) {
        *self.identity_store.lock().unwrap() = store;
    }

    /// The custody door as wired now (never held across an await).
    fn identity_store(&self) -> Arc<dyn AtprotoIdentityStore> {
        Arc::clone(&self.identity_store.lock().unwrap())
    }

    /// Wire where the minted credentials' secrets rest
    /// ([`AtprotoCredentialStore`] — the account plane's `fauna.state.atproto`).
    /// Post-construction, the `set_*` seam pattern — never over UniFFI
    /// (`Arc<dyn AtprotoCredentialStore>` has no FFI ABI): the native seats
    /// pass `fauna_account_seams::atproto_credentials::RuntimeAtprotoCredentials`
    /// over their handle source, web's ATProto chunk the account port's
    /// forwarder ([`crate::port::PortAtprotoCredentials`]). Wire it before the
    /// first `refresh()`; until then every credential read and write is
    /// refused — no row is revealable, and a mint's secret is handed back
    /// once but flagged as not kept.
    pub fn set_credential_store(&self, credentials: Arc<dyn AtprotoCredentialStore>) {
        *self.credentials.lock().unwrap() = Some(credentials);
    }

    /// Wire the consent-time grant's seams ([`ConsentGrantSeams`]) — the
    /// `set_*` seam pattern, never over UniFFI. Without them an approve of a
    /// records consent is refused with the page error.
    pub fn set_consent_grant_seams(&self, seams: Arc<ConsentGrantSeams>) {
        *self.consent_grants.lock().unwrap() = Some(seams);
    }

    /// The wired credential seam, or the refusal an unwired one answers with
    /// (`read` picks the arm).
    fn credential_store(&self, read: bool) -> Result<Arc<dyn AtprotoCredentialStore>, StoreError> {
        self.credentials.lock().unwrap().clone().ok_or_else(|| {
            let why = crate::credentials::NOT_WIRED.to_string();
            if read {
                StoreError::Load(why)
            } else {
                StoreError::Save(why)
            }
        })
    }

    /// Every credential this account holds a secret for.
    async fn held_credentials(&self) -> Result<Vec<AtprotoAppCredential>, StoreError> {
        Ok(self
            .credential_store(true)?
            .atproto()
            .await?
            .app_credentials)
    }

    /// The S4-C genesis-seniority check (`atproto-pds-bridge.md` § State &
    /// data shape). Runs after every status convergence; quiet unless the
    /// public directory is readable AND contradicts this client's stored
    /// senior key. Not in the uniffi export block — clients never drive it
    /// directly.
    async fn check_custody(&self) {
        let Some((did, handle)) = ({
            let s = self.state.lock().unwrap();
            s.custody_did
                .as_ref()
                // Only did:plc has a directory to audit; did:web custody is
                // domain custody (out of scope, stated in the goal doc). The
                // SHARED predicate — the sweep asks the same question the same
                // way since the fix; they disagreed before it.
                .filter(|d| fauna_client_atproto::genesis_verify::is_auditable_did(d))
                .map(|d| {
                    (
                        d.clone(),
                        s.identity
                            .as_ref()
                            .map(|i| i.handle.clone())
                            .unwrap_or_default(),
                    )
                })
        }) else {
            return;
        };

        // The comparison set: the whole held rotation keyring — every entry
        // is user-custodied (keys enter the account plane's custody only
        // through the client-side sole writer), and which held key a DID
        // publishes senior is that DID's own log's fact (fresh-key-per-mint
        // keeps retired identities' burned keys in the ring beside the live
        // one's). Empty (e.g. another device minted and its rows haven't
        // synced here yet) or unreadable ⇒ cannot verify — quiet, retried on
        // a later refresh.
        let held: Vec<String> = match self.identity_store().atproto_identity().await {
            Ok(custody) => custody
                .rotation_keys
                .iter()
                .map(|k| k.pubkey_did_key.clone())
                .collect(),
            Err(_) => return,
        };
        if held.is_empty() {
            return;
        }

        // Cache key: the ring, not one key — a fresh re-mint key syncing in
        // must re-verify, not inherit a stale pass.
        let pair = (did.clone(), held.join(","));
        if self.state.lock().unwrap().custody_ok.contains(&pair) {
            return;
        }

        match self.verifier.verify(did.clone(), held).await {
            Ok(SeniorityVerdict::Verified {
                observed_seniors, ..
            }) => {
                {
                    let mut s = self.state.lock().unwrap();
                    s.custody_ok.insert(pair);
                    s.custody_suspect = None;
                    s.custody_alarmed = None;
                }
                if let Some(alerts) = &self.alerts {
                    alerts.clear(&fauna_client_atproto::genesis_verify::alert_key(&did));
                }
                // Converge the published-for burn: the directory's own log
                // just said these held keys are senior for this DID, which is
                // exactly the fact that keeps `mint_rotation_key` from ever
                // reusing them (idempotent; failure is quiet — the next
                // Verified pass, or the retire path's own write, re-records).
                for senior in &observed_seniors {
                    if let Err(e) =
                        record_published_binding(&*self.identity_store(), &did, senior).await
                    {
                        tracing::warn!(%did, %e, "could not record the rotation-key publication; will re-derive");
                    }
                }
            }
            Ok(SeniorityVerdict::Mismatch(reason)) => {
                let fingerprint = format!("{reason:?}");
                let confirmed = {
                    let mut s = self.state.lock().unwrap();
                    let confirmed = s
                        .custody_suspect
                        .as_ref()
                        .is_some_and(|(d, f)| d == &did && f == &fingerprint);
                    s.custody_suspect = Some((did.clone(), fingerprint));
                    if confirmed {
                        s.custody_alarmed = Some(did.clone());
                    }
                    confirmed
                };
                // One-convergence debounce: the first sighting is recorded,
                // not alarmed — a sibling device's fresh re-mint key can be
                // published before this device's custody rows sync it,
                // and that contradiction dissolves with the sync. A real
                // compromise survives into the next convergence (PLC's 72 h
                // window dwarfs one pass); a self-clearing critical-alert
                // flash would teach the user to ignore the real one.
                if confirmed {
                    if let Some(alerts) = &self.alerts {
                        // Shared with the session-start sweep's own call, so the
                        // two callers can never drift on the key or the copy.
                        fauna_client_atproto::genesis_verify::sync_custody_alert(
                            alerts,
                            &did,
                            &handle,
                            &SeniorityVerdict::Mismatch(reason),
                        );
                    } else {
                        tracing::error!(
                            ?reason,
                            %did,
                            "ATProto genesis-seniority MISMATCH: the published senior rotation key is not this client's"
                        );
                    }
                } else {
                    tracing::warn!(
                        ?reason,
                        %did,
                        "ATProto genesis-seniority contradiction seen once; re-checking next convergence before alarming"
                    );
                }
            }
            Err(failure) => {
                // Unreachable ≠ compromised: never an alarm, retried next
                // refresh (genesis_verify verdict philosophy).
                tracing::warn!(%did, %failure, "ATProto genesis-seniority check could not read the directory; will retry");
            }
        }
    }

    /// The S5 slice-5b converge pass: publish the PLC tombstone the user opted
    /// into, once the delete-presence sweep has finished, and report the
    /// outcome (`atproto-pds-bridge.md` § Disable & revocation layer 2).
    ///
    /// A **converge** pass, not a gesture, because the act spans a crash: the
    /// tick that records the intent cannot also submit, since the submission
    /// has to wait for a sweep the user is not sitting through. Nest holds the
    /// intent durably (`tombstone_requested`); this reads it back on every
    /// status convergence and finishes the job — which is also what lets a
    /// *second* device complete a retirement the first one started.
    ///
    /// Runs after [`Self::check_custody`], on the same convergence, and is
    /// quiet in every outcome but the terminal one: it is a background act, and
    /// the page's own error line belongs to what the user just did.
    ///
    /// Ordering is the load-bearing part. The sweep is observed finished
    /// *before* anything is submitted, because a tombstoned DID stops
    /// resolving and a relay that cannot resolve a DID cannot verify that DID's
    /// commits — retiring first would strand the delete tombstones, leaving our
    /// repo gone and every network copy standing. The observable is the DID's
    /// own PDS answering `RepoNotFound`, read client-direct, so **the box
    /// cannot assert that its own sweep completed**. That is a politeness
    /// ordering rather than a security boundary — a hostile box could equally
    /// just not sweep — and what actually protects the user is the tombstone
    /// itself, which lands regardless.
    async fn converge_tombstone(&self) {
        // The intent is nest's, so the trigger is nest's status read: opted in,
        // presence already recorded deleted, and a did:plc (did:web has no
        // operation log to tombstone — its custody IS domain custody).
        let Some(did) = ({
            let s = self.state.lock().unwrap();
            (s.tombstone_requested && s.identity.as_ref().is_some_and(|i| i.status == "deleted"))
                .then(|| s.custody_did.clone())
                .flatten()
                .filter(|d| d.starts_with("did:plc:"))
        }) else {
            return;
        };

        // Only a held rotation key can sign the op, and the ring lives only
        // here. Which held key may sign for THIS DID is the published log's
        // fact — the retire path selects the one the standing head lists
        // (after a fresh-key re-mint the ring holds several). Empty (another
        // device minted and its rows have not synced to this one yet) ⇒
        // this device cannot act; another can, and the durable intent is what
        // keeps that true.
        let (ring, consented) = match self.identity_store().atproto_identity().await {
            Ok(custody) => {
                let consented = custody.tombstone_consents.iter().any(|d| d == &did);
                (custody.rotation_keys, consented)
            }
            Err(_) => return,
        };
        if ring.is_empty() {
            return;
        }
        // The intent alone is nest testimony, and a terminal, irreversible act
        // must never run on nest testimony alone:
        // the client acts only when the nest's durable intent AGREES with a
        // consent record the nest cannot author — written into
        // the custody's `tombstone_consents` by [`Self::request_tombstone`]
        // at the moment the user ticked the opt-in, and synced with the ring,
        // which is what still lets a sibling device finish the retirement.
        // Intent without consent is either a hostile box's fabricated flag or
        // a not-yet-synced config — the first must be refused and the second
        // resolves itself, so both wait here, loudly.
        if !consented {
            tracing::warn!(
                %did,
                "nest records a tombstone intent this client has no user consent for; refusing to sign"
            );
            return;
        }

        // One converge step off ONE read of the DID's published log — sweep
        // observation, already-retired detection, key selection, submit. Two
        // separate seam calls used to hide a deadlock: the head that means
        // "already retired" is a tombstone with no PDS service to probe, so a
        // sweep-first shape could never reach the AlreadyRetired report and a
        // report that failed after a successful submit wedged the nest row
        // forever.
        let prev_cid = match self.retirement.converge(did.clone(), ring).await {
            Ok(RetirementProgress::Retired {
                prev_cid,
                signed_with_did_key,
            }) => {
                // Burn the key that just signed for this DID: the retired
                // log carries it forever, so a later mint reusing it would
                // publicly link the fresh identity to this dead one. This
                // write is the burn's guaranteed leg (the custody check's
                // Verified arm records the same fact during the identity's
                // life, but only this path is certain to have run by the
                // time a re-mint is possible). Failure is a warn, not a
                // wedge: the retirement itself is published and must be
                // reported regardless.
                if !signed_with_did_key.is_empty()
                    && let Err(e) = record_published_binding(
                        &*self.identity_store(),
                        &did,
                        &signed_with_did_key,
                    )
                    .await
                {
                    tracing::warn!(%did, %e, "could not burn the retired identity's rotation key; a later custody pass may re-record it");
                }
                prev_cid
            }
            // The directory already holds a tombstone — a previous attempt, or
            // another of this user's devices, got there first (possibly this
            // client crashing between submit and report). Still reported,
            // because nest learns the outcome only from a client; that report
            // is exactly what closes the crash window. Empty `prev_cid`: this
            // client chained nothing itself.
            Ok(RetirementProgress::AlreadyRetired) => String::new(),
            Ok(RetirementProgress::SweepStillRunning) => {
                // Not an error: the bridge converges on the `deleted` status on
                // its own poll, so this simply has not finished yet.
                tracing::debug!(%did, "bluesky retirement waiting: the presence sweep is still running");
                return;
            }
            Err(failure) => {
                // Unanswered is never a licence to retire early — the same
                // quiet-retry philosophy as the seniority check.
                tracing::warn!(%did, %failure, "bluesky retirement could not converge; will retry");
                return;
            }
        };

        match self.nest_api.record_tombstone(prev_cid).await {
            Ok(newly_tombstoned) => {
                tracing::info!(
                    %did,
                    newly_tombstoned,
                    "ATProto identity retired: the PLC tombstone is published and recorded"
                );
                // The identity's status just changed under the already-painted
                // page. Applied locally and repainted rather than re-running
                // `refresh` — which would re-enter this pass — and the local
                // value is not a guess: it is what nest just recorded.
                {
                    let mut s = self.state.lock().unwrap();
                    s.tombstone_requested = false;
                    if let Some(id) = &mut s.identity {
                        id.status = "tombstoned".into();
                    }
                }
                self.observer.on_changed();
            }
            // The op IS published — the directory is the durable record, not
            // this call — so a failed report is a reporting gap, never a lost
            // retirement. The intent still stands nest-side, so the next
            // convergence re-reads an already-tombstoned log and reports again.
            Err(e) => {
                tracing::warn!(
                    %did,
                    detail = e.detail(),
                    "the PLC tombstone is published but nest did not record it; will re-report"
                );
            }
        }
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl AtprotoSettingsMachine {
    // ── Read surface ────────────────────────────────────────────────────

    /// The whole renderable ATProto settings surface in one record. Never
    /// carries a secret — see `crate::snapshots`.
    pub fn snapshot(&self) -> AtprotoSettingsSnapshot {
        let s = self.state.lock().unwrap();
        AtprotoSettingsSnapshot {
            level: s.level.as_str().to_string(),
            hosted_allowed: s.hosted_allowed,
            hosted_gate_reason: (!s.hosted_allowed).then(|| {
                LocalizedText::key_arg(GATE_REASON_KEY, "domain", s.handle_domain.clone())
            }),
            handle_preview: s.handle_preview.clone(),
            identity: s.identity.clone(),
            link: s.link.clone(),
            pending_transition: s.pending.as_ref().map(|p| TransitionCardModel {
                target_level: p.target.as_str().to_string(),
                lines: compose_card_lines(&p.plan, &s),
                show_history_backfill: p.plan.mints_identity,
                in_progress: p.in_progress,
            }),
            did_method: s.did_method.clone(),
            // After mint the method is a fact, displayed not chosen
            // (`ui/atproto.md` § Reveal/greying rules).
            show_did_method_radio: s.identity.is_none(),
            history_backfill: s.history_backfill,
            // Reachable at any *standing* status so the stronger action
            // survives a step-down (§ Layout & flow item 4) — and withdrawn
            // once the presence is already destroyed, where the gesture's only
            // outcome is a no-op.
            show_delete_presence: s.identity.as_ref().is_some_and(presence_stands),
            // Derived from BOTH, so the ceremony structurally cannot outlive
            // the presence it would destroy: the convergence after a successful
            // sweep clears the card without the gesture having to.
            //
            // The one exception is `swept`: a ticked retirement that failed
            // after its sweep landed keeps the card up for the retry.
            delete_confirm: s
                .delete_confirm
                .as_ref()
                .filter(|open| open.swept || s.identity.as_ref().is_some_and(presence_stands))
                .map(|open| DeleteConfirmCardModel {
                    lines: compose_delete_confirm_lines(&s, open.retire_identity),
                    in_progress: open.in_progress,
                    retire_identity: retire_opt_in(&s, open),
                }),
            credentials: s.credentials.clone(),
            sessions: s.sessions.clone(),
            external_apps_enabled: s.external_apps_enabled,
            delegation: s.delegation.clone(),
            consents: s.consents.clone(),
            contest: s.contest.as_ref().map(|(_, card)| card.clone()),
            // Derived from BOTH, so the ceremony structurally cannot outlive
            // the violation it confirms: a convergence that clears `contest`
            // takes the open confirm card with it without having to remember
            // to.
            contest_confirm: s.contest.as_ref().zip(s.contest_confirm.as_ref()).map(
                |((_, card), open)| ContestConfirmCardModel {
                    lines: compose_contest_confirm_lines(card, &s),
                    in_progress: open.in_progress,
                },
            ),
            error: s.error.clone(),
        }
    }

    // ── Gestures ────────────────────────────────────────────────────────

    /// Re-read the page: the integration status (`get_integration_status`),
    /// the consume-side link summary (`fauna.bridges.list`), credential rows +
    /// kill-switch state (`list_app_credentials`), live sessions
    /// (`list_sessions`), and the local `fauna.state.atproto` custody that decides which rows
    /// are revealable.
    ///
    /// Each read degrades independently — a sessions failure must not blank
    /// the credential list, which is the half the user needs to revoke with.
    /// On any failure the prior data for *that* part is kept and the page
    /// error is set; a fully clean pass clears the error. Notifies once, at
    /// the end, so the page paints one consistent state rather than partial
    /// ones.
    pub async fn refresh(&self) {
        // Which credential_ids does the account hold a secret for? Fetched
        // before the rows so the join below is against one consistent read.
        // An unreadable store is not a page error — a web tab that hosts no
        // account runtime (only the MLS-writing tab does) and a seat whose
        // runtime is still assembling both land here — it hides every reveal
        // (below) and is logged, the blessing door's posture for the same
        // absence.
        let local_ids: Option<Vec<String>> = match self.held_credentials().await {
            Ok(held) => Some(held.into_iter().map(|c| c.credential_id).collect()),
            Err(e) => {
                tracing::warn!("app credentials unreadable, rendered unrevealable: {e}");
                None
            }
        };

        match self.nest_api.list_app_credentials().await {
            Ok(listing) => {
                let rows: Vec<AppCredentialRow> = listing
                    .credentials
                    .into_iter()
                    .map(|c| AppCredentialRow {
                        // `revealable` fails **safe**: when the config read
                        // failed we cannot prove a local secret exists, so the
                        // reveal affordance hides rather than offering a reveal
                        // that would then error.
                        revealable: local_ids
                            .as_ref()
                            .is_some_and(|ids| ids.iter().any(|id| id == &c.credential_id)),
                        credential_id: c.credential_id,
                        label: c.label,
                        dm_allowed: c.dm_allowed,
                        created_at_millis: c.created_at_millis,
                        last_used_at_millis: c.last_used_at_millis,
                    })
                    .collect();
                let mut s = self.state.lock().unwrap();
                s.credentials = rows;
                s.external_apps_enabled = listing.external_apps_enabled;
                s.error = None;
            }
            Err(e) => self.set_error(REFRESH_ERROR_KEY, e.detail()),
        }

        // The connected-apps rows are TWO reads joined here: the session says a
        // credential family is live and when it expires, the grant says which
        // client it is and what the user approved it for. One identifier joins
        // them — the nest writes `atproto_oauth_grants.grant_id` and
        // `atproto_sessions.session_id` as the same bytes in one transaction —
        // so this is a lookup, not a heuristic.
        //
        // Composing in the machine (rather than handing both lists to seven
        // painters) is priority #2 *and* the security fence: `client_name` is
        // attacker-supplied text, and stripping it once here holds for every
        // app at once.
        //
        // The grants read degrades independently, and the direction matters: a
        // grants failure leaves the sessions rendered *without* their richer
        // half, which is the F1 row the user could already revoke with. Letting
        // it blank the list would take away the affordance the page exists for
        // at exactly the moment something is wrong.
        match self.nest_api.list_sessions().await {
            Ok(sessions) => {
                let grants = match self.nest_api.list_grants().await {
                    Ok(grants) => grants,
                    Err(e) => {
                        self.set_error(REFRESH_ERROR_KEY, e.detail());
                        Vec::new()
                    }
                };
                // Composed once, in `list_grants` order (`ORDER BY created_at
                // ASC`), and kept in that order: a slot is `take`n as the
                // session pass claims it, so what remains at the end is the
                // suspended set, still ordered. A `HashMap` drain would put the
                // list in a different order on every refresh.
                let mut composed: Vec<Option<ComposedGrant>> = grants
                    .into_iter()
                    .map(|g| {
                        Some(ComposedGrant {
                            hex_id: hex::encode(&g.grant_id),
                            suspended: g.suspended,
                            // The grant's OWN creation stamp, kept because a
                            // suspended row has no session to read it from.
                            // `AtprotoGrantRow` deliberately does not carry it
                            // (one fact, one source), so it rides here.
                            created_at_millis: g.created_at,
                            row: grant_row(g),
                        })
                    })
                    .collect();
                let mut by_id: HashMap<String, usize> = composed
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| c.as_ref().map(|c| (c.hex_id.clone(), i)))
                    .collect();
                let mut rows: Vec<AtprotoSessionRow> = sessions
                    .into_iter()
                    .map(|mut s| {
                        // `client_note` is attacker text too, and on the OAuth
                        // plane it is the SAME string as `client_name`: the
                        // nest's one-transaction write copies the resolved
                        // client name into both `atproto_oauth_grants` and
                        // `atproto_sessions.client_note`. Stripping the grant
                        // half alone therefore closed nothing — the painter
                        // reads this field (proven
                        // against the shipped tui list, where forged newlines
                        // produced the list's OWN "No connected apps" empty
                        // state above a live grant).
                        //
                        // Strip rather than refuse, by the audience test: this
                        // is text whose job is to be *read* (a display name),
                        // not *compared* (an identity, a URL, a code). That is
                        // the opposite call from `client_id`, which is
                        // self-authenticating and must be refused rather than
                        // mangled.
                        s.client_note = s.client_note.map(|n| strip_control_chars(&n).into_owned());
                        s.grant = by_id
                            .remove(&s.session_id_hex)
                            .and_then(|i| composed[i].take())
                            .map(|c| c.row);
                        // Reached from the session side, so a live session
                        // exists by construction — the flag would contradict
                        // the row it sits on.
                        s.suspended = false;
                        s
                    })
                    .collect();
                // What is LEFT is a grant with no live session: the suspended
                // set. Draining it here is what makes the OAuth half of this
                // composition grant-primary, while the app-credential half
                // stays session-primary — those sessions have no grant row at
                // all, so inverting the whole join would lose them instead of
                // fixing anything.
                rows.extend(composed.into_iter().flatten().map(|c| AtprotoSessionRow {
                    // The grant id IS the session-family id `revoke_session`
                    // takes, which is what keeps this row individually
                    // revocable with no `revoke_grant` companion — the matrix
                    // requires exactly that, and it needs no new mechanism.
                    session_id_hex: c.hex_id,
                    plane: OAUTH_PLANE.to_string(),
                    credential_id: None,
                    // The session that carried the note is gone. The grant's
                    // resolved `client_name` is the identity half and already
                    // rides `grant`; synthesizing a note here would be a
                    // second, weaker name for one app.
                    client_note: None,
                    created_at_millis: c.created_at_millis,
                    last_refreshed_at_millis: None,
                    expires_at_millis: None,
                    grant: Some(c.row),
                    suspended: c.suspended,
                }));
                self.state.lock().unwrap().sessions = rows;
            }
            Err(e) => self.set_error(REFRESH_ERROR_KEY, e.detail()),
        }

        // The selector reads run AFTER the credential listing on purpose: a
        // clean listing clears the page error, so an error set by these later
        // reads survives to the paint (the same reason the sessions read sits
        // where it does).
        match self.nest_api.get_integration_status().await {
            Ok(status) => {
                let level = IntegrationLevel::from_wire(&status.level);
                let departed_did = {
                    let mut s = self.state.lock().unwrap();
                    if let Some(level) = level {
                        s.level = level;
                    }
                    s.hosted_allowed = status.hosted_allowed;
                    s.handle_domain = status.handle_domain;
                    s.handle_preview = status.handle_preview;
                    let new_did = status.identity.as_ref().and_then(|i| i.did.clone());
                    // A DID leaving the roster (retirement archived it, or a
                    // re-mint replaced it) takes its custody *bookkeeping* with
                    // it: a suspect for it is moot, since nothing on this page
                    // re-checks a DID that is no longer the account's identity.
                    // Its posted **alarm** is a different question, and one the
                    // nest does not get to answer — see
                    // [`Self::clear_departed_alarm_if_earned`].
                    let departed = match (&s.custody_did, &new_did) {
                        (Some(old), new) if new.as_ref() != Some(old) => Some(old.clone()),
                        _ => None,
                    };
                    if let Some(old) = &departed
                        && s.custody_suspect.as_ref().is_some_and(|(d, _)| d == old)
                    {
                        s.custody_suspect = None;
                        s.custody_alarmed = None;
                    }
                    s.custody_did = new_did;
                    s.tombstone_requested = status
                        .identity
                        .as_ref()
                        .is_some_and(|i| i.tombstone_requested);
                    s.identity = status.identity.map(identity_row);
                    departed
                };
                // Freeze whatever the nest just named, so it cannot later
                // retract the claim to dodge the audit. Same
                // converge writer the sweep uses; idempotent, and quiet on
                // failure — the next convergence or sweep re-records.
                // Bound to a local FIRST: holding the `State` guard across the
                // await below makes this future non-`Send`, which the uniffi
                // async exports reject (and a default-feature `--lib` test run
                // never compiles).
                let just_named = self.state.lock().unwrap().custody_did.clone();
                if let Some(did) = just_named
                    && let Err(e) = record_nest_named_did(&*self.identity_store(), &did).await
                {
                    tracing::warn!(%did, %e, "could not freeze the nest-named DID for the audit floor; will re-record");
                }
                if let (Some(old), Some(alerts)) = (departed_did, &self.alerts) {
                    self.clear_departed_alarm_if_earned(&old, alerts).await;
                }
                // An unknown level from a NEWER nest must stay a visible
                // refusal, never a silent step-down to Off
                // (`IntegrationLevel::from_wire`) — the prior level is kept
                // and the page error says so.
                if level.is_none() {
                    self.set_error(
                        REFRESH_ERROR_KEY,
                        &format!("unknown level: {}", status.level),
                    );
                }
            }
            Err(e) => self.set_error(REFRESH_ERROR_KEY, e.detail()),
        }

        match self.nest_api.bluesky_link_status().await {
            Ok(link) => self.state.lock().unwrap().link = link.map(link_row),
            Err(e) => self.set_error(REFRESH_ERROR_KEY, e.detail()),
        }

        self.refresh_delegation().await;

        self.refresh_consents().await;

        self.observer.on_changed();

        // AFTER the paint: the custody check does its own (network) read and
        // reports through the critical-alerts registry, whose observer
        // repaints — the page must never wait on the directory.
        self.check_custody().await;
        // Same convergence, same reason for running after the paint: the
        // retirement pass talks to the directory and to the DID's own PDS, and
        // a page load must not wait on either.
        self.converge_tombstone().await;
        // Last, and after the custody check by design: the contest answers the
        // violation that check finds, so running it here means a single
        // convergence can detect, render the remedy, and — if the user has
        // already consented — perform it.
        self.converge_contest().await;
    }

    /// The recovery-fork converge pass: compose the contest card from the
    /// directory's own log, and, when the user's scoped consent covers the
    /// violation standing there *now*, build, sign and submit the fork
    /// (`atproto-pds-bridge.md` § State & data shape, the recovery-fork
    /// contest).
    ///
    /// **A converge pass, not a gesture** — the same reason retirement is one:
    /// the act spans a crash and a device boundary. The gesture that records
    /// the consent may not be the pass that submits (a submit can fail, a
    /// device can die mid-flight, a sibling holding the synced ring can
    /// finish), so everything is re-derived from a fresh log read every pass
    /// and nothing client-side remembers "already contested" — the directory
    /// is the durable record.
    ///
    /// **It never contests on its own** (decision 6, iron-clad). This pass is
    /// the *executor* of a consent, never its author: with no matching intent
    /// it composes the card and stops. An auto-contester would be a detector
    /// promoted to an actor able to nullify the user's own out-of-band
    /// operations.
    ///
    /// Quiet in every outcome, including success: the page's `error-message`
    /// belongs to what the user just did, and the contest's real report is the
    /// custody alarm coming down on the next convergence — off the directory's
    /// evidence, with no new mechanism (decision 8).
    async fn converge_contest(&self) {
        self.converge_contest_reporting(false).await;
    }

    /// Drop the contest notice and, with it, any ceremony standing on it.
    fn clear_contest(&self) {
        let mut s = self.state.lock().unwrap();
        s.contest = None;
        s.contest_confirm = None;
    }

    /// Take a departed DID's standing custody alarm down **only on a
    /// client-side reason**.
    ///
    /// # The finding
    ///
    /// This clear used to fire on one input: the nest's `identity` field going
    /// `null`. Composed with the session-start sweep's matching silence, that
    /// made a *detector* switchable by the party it detects — leg B took a
    /// standing custody banner down now, and leg A kept it from coming back,
    /// for as long as the box kept answering `identity: null`. Each half is
    /// defensible alone; together they are the finding.
    ///
    /// # What counts as a reason
    ///
    /// Never the nest's silence. Two things, both of which the client can
    /// establish without it:
    ///
    /// 1. **The DID is absent from the held ring's `published_for_dids`.** That
    ///    field is written only from the directory's own log or from having
    ///    just signed for it (`atproto-pds-bridge.md` § State & data shape,
    ///    decision (b)), so its absence means the nest's testimony was the only
    ///    thing that ever tied this DID to the account. With that withdrawn
    ///    there is no independent basis left to keep accusing — this is the
    ///    genesis-time TOFU failure, and the case
    ///    `an_alarm_for_a_departed_did_is_cleared_on_the_next_refresh` has
    ///    always defended.
    /// 2. **The public log says the identity is terminally retired.** A
    ///    tombstone is the condition being re-checked and found resolved
    ///    (`critical-alerts.md` § Mechanism → *Lifetime*), and it is the
    ///    evidence a retirement this client performed leaves behind — so "the
    ///    client itself retired it" needs no separate stored flag. It also
    ///    covers a retirement a *sibling device* performed, which such a flag
    ///    could not.
    ///
    /// Everything else leaves the banner up: a live log whose senior key is not
    /// ours is the attack, and an unreadable directory is not a resolution
    /// (unreachable is never resolved — the next refresh retries). The cost of
    /// being wrong in that direction is a stale banner the user can reason
    /// about; the other direction is the finding.
    ///
    /// Both reads are paid **only** when an alarm is actually standing for the
    /// departed DID, which is rare — a DID change with a live accusation
    /// against it.
    async fn clear_departed_alarm_if_earned(&self, old: &str, alerts: &Arc<CriticalAlerts>) {
        let key = fauna_client_atproto::genesis_verify::alert_key(old);
        if !alerts.active().iter().any(|row| row.key == key) {
            return;
        }

        let identity = match self.identity_store().atproto_identity().await {
            Ok(custody) => custody,
            Err(e) => {
                tracing::warn!(
                    did = %old, %e,
                    "could not read the rotation keyring to judge a departed DID's alarm; \
                     it stands until a refresh can"
                );
                return;
            }
        };
        let held = identity.rotation_keys;

        // The clear needs a client-side reason, and BOTH floor sources are
        // one. `published_for_dids` alone was not enough: it is
        // written only on a *passing* verdict, so the genesis-time compromise —
        // where the verdict never passes — read as "nest testimony was the only
        // tie" and cleared the very alarm that case exists to raise. A DID the
        // client froze when the nest named it is still the client's own record,
        // so its alarm survives the nest going quiet.
        //
        // Keeping these two reads separate is deliberate: the sweep merges them
        // because it only asks "which DIDs do I fetch?", while the question
        // here — "did anything but this box ever tie this DID to the account?"
        // — is answered by either one independently.
        if !held
            .iter()
            .any(|k| k.published_for_dids.iter().any(|d| d == old))
            && !identity.nest_named_dids.iter().any(|d| d == old)
        {
            tracing::debug!(
                did = %old,
                "departed DID was never observed published for a held key and was never \
                 frozen from a nest claim; its alarm rested on nest testimony alone and \
                 goes with it"
            );
            alerts.clear(&key);
            return;
        }

        let ring: Vec<String> = held.iter().map(|k| k.pubkey_did_key.clone()).collect();
        match self.verifier.verify(old.to_string(), ring).await {
            Ok(verdict) if verdict.is_terminal_retirement() => {
                tracing::info!(did = %old, "departed DID is terminally retired in the public log; alarm cleared");
                alerts.clear(&key);
            }
            Ok(_) => tracing::warn!(
                did = %old,
                "the nest stopped naming a DID this client independently protects, but its \
                 published log is still live — the custody alarm STANDS"
            ),
            Err(e) => tracing::warn!(
                did = %old, %e,
                "could not read a departed DID's published log; unreachable is not resolved, \
                 so its custody alarm stands"
            ),
        }
    }

    // ── The depth selector (`ui/atproto.md` § Layout & flow) ────────────

    /// Select a target level (`atproto-depth-*`). Never mutates the level by
    /// itself: an effectful move stages the transition card for an explicit
    /// confirm; the one effect-free move (Off → Linked) applies immediately,
    /// per the ratified matrix. Re-selecting the current level closes any
    /// staged card.
    ///
    /// A gated hosted selection is refused loudly (the page error carries the
    /// gate reason) rather than dropped — the options render disabled, but a
    /// command that does arrive must not vanish silently (testing.md rule 11).
    pub async fn select_level(&self, target_level: String) {
        let Some(target) = IntegrationLevel::from_wire(&target_level) else {
            self.set_error(
                TRANSITION_ERROR_KEY,
                &format!("unknown level: {target_level}"),
            );
            self.observer.on_changed();
            return;
        };

        let staged = {
            let mut s = self.state.lock().unwrap();
            if s.pending.as_ref().is_some_and(|p| p.in_progress) {
                // A confirm is mid-flight; the selector is inert until it
                // resolves (the refresh that follows repaints the truth).
                return;
            }
            if target == s.level {
                s.pending = None;
                None
            } else if target.is_hosted() && !s.hosted_allowed {
                let domain = s.handle_domain.clone();
                drop(s);
                self.set_error_localized(LocalizedText::key_arg(GATE_REASON_KEY, "domain", domain));
                self.observer.on_changed();
                return;
            } else {
                let plan = TransitionPlan::for_move(s.level, target, s.level_context());
                let apply_now = plan.is_effect_free();
                // Off → Linked applies immediately on select — still a nest
                // write (the level is nest state), just card-less; every
                // other move waits for the card's confirm.
                s.pending = Some(PendingTransition {
                    target,
                    plan,
                    in_progress: apply_now,
                });
                apply_now.then_some(target)
            }
        };

        match staged {
            Some(_) => self.perform_staged_transition().await,
            None => self.observer.on_changed(),
        }
    }

    /// Confirm the staged transition (`atproto-depth-confirm`) — the ONE nest
    /// call per confirmed level change. On failure the card stays open with
    /// the page error populated and the level unchanged; retry is safe
    /// (`ui/atproto.md` § Errors & edge cases).
    pub async fn confirm_transition(&self) {
        {
            let mut s = self.state.lock().unwrap();
            let Some(p) = s.pending.as_mut() else {
                return;
            };
            if p.in_progress {
                return; // double-confirm guard; the in-flight call decides
            }
            p.in_progress = true;
        }
        self.observer.on_changed(); // paint the progress state
        self.perform_staged_transition().await;
    }

    /// Close the card without changes (`atproto-depth-cancel`). Inert while
    /// the confirm is in flight — the call is already on the wire, and the
    /// refresh it triggers repaints the truth.
    pub fn cancel_transition(&self) {
        let mut s = self.state.lock().unwrap();
        if s.pending.as_ref().is_some_and(|p| p.in_progress) {
            return;
        }
        s.pending = None;
        drop(s);
        self.observer.on_changed();
    }

    /// Choose the mint's DID method (`atproto-did-method-*`; pre-mint only —
    /// after mint the method is a fact).
    pub fn set_did_method(&self, method: String) {
        if method != "plc" && method != "web" {
            self.set_error(TRANSITION_ERROR_KEY, &format!("unknown method: {method}"));
            self.observer.on_changed();
            return;
        }
        self.state.lock().unwrap().did_method = method;
        self.observer.on_changed();
    }

    /// The history-backfill opt-in (`atproto-history-backfill`; rides the
    /// minting transition's card — the second explicit consent).
    pub fn set_history_backfill(&self, enabled: bool) {
        self.state.lock().unwrap().history_backfill = enabled;
        self.observer.on_changed();
    }

    /// Mint a new app credential for `label` and return its secret **once**.
    ///
    /// The generated secret is the only copy the user will be shown at mint
    /// time; it is persisted to the account plane's `fauna.state.atproto`
    /// ([`AtprotoCredentialStore::put_app_credential`]) so a later
    /// [`Self::reveal_secret`] — on this device or any of the account's — can
    /// recover it, but the nest never sees it (D3). `dm_allowed` selects the
    /// ecosystem's DM-privileged scope.
    ///
    /// The persist waits out an account runtime still assembling (the seam's
    /// own rule); a runtime that never answers is the partial success below.
    ///
    /// **Returns the secret even if the local persist fails**, with the page
    /// error set to explain. At that point the credential is already live on
    /// the nest, so the alternatives are worse: swallowing the secret would
    /// leave a working credential nobody can use *or* re-reveal (recoverable
    /// only by revoking it), while handing it back lets the user copy it into
    /// their app right now and lose nothing but the ability to re-reveal it
    /// later. Callers must therefore surface `snapshot().error` alongside the
    /// reveal, not instead of it.
    pub async fn mint(
        &self,
        label: String,
        dm_allowed: bool,
    ) -> Result<SecretString, AtprotoSettingsError> {
        // The held credentials, for the collision set. An unreadable store
        // (no runtime yet) contributes nothing: the nest's listing already
        // names every LIVE credential, which is all a collision can be with —
        // the store's rows are a subset of it but for a secret whose revoke
        // raced. The persist below waits out a runtime still assembling.
        let held = match self.held_credentials().await {
            Ok(held) => held,
            Err(e) => {
                tracing::warn!("app credentials unreadable before a mint: {e}");
                Vec::new()
            }
        };

        // Collision-avoid against **both** sources: the nest knows every
        // credential that exists (including ones minted on a sibling device
        // whose rows have not synced here), the account store knows every one
        // this account holds a secret for. Deriving from either alone can mint
        // a duplicate id, which the nest would either reject or — worse —
        // collapse onto an existing row. A revoked id is in neither (the nest
        // deletes its row, the plane tombstones its own), so it may be minted
        // again: the fresh put's later stamp outranks the tombstone.
        let mut existing: Vec<String> = held.iter().map(|c| c.credential_id.clone()).collect();
        {
            let s = self.state.lock().unwrap();
            for row in &s.credentials {
                if !existing.contains(&row.credential_id) {
                    existing.push(row.credential_id.clone());
                }
            }
        }
        let credential_id = derive_credential_id(&label, &existing);

        let secret = generate_app_credential();
        let verifier = compute_app_credential_verifier(&secret);

        if let Err(e) = self
            .nest_api
            .provision_app_credential(credential_id.clone(), label.clone(), verifier, dm_allowed)
            .await
        {
            self.set_error(MINT_ERROR_KEY, e.detail());
            self.observer.on_changed();
            return Err(AtprotoSettingsError::Nest {
                detail: e.detail().to_string(),
            });
        }

        // Nest-side row is live from here on: every path below must still hand
        // the caller the secret.
        let credential = AtprotoAppCredential {
            credential_id,
            label,
            secret: SecretByteBuf::new(secret.as_str().as_bytes().to_vec()),
            dm_allowed,
            created_at: Timestamp::now_secs() as u64,
        };
        let saved = match self.credential_store(false) {
            Ok(store) => store.put_app_credential(credential).await,
            Err(e) => Err(e),
        };

        match saved {
            Ok(_) => {
                self.refresh().await;
            }
            Err(e) => {
                // Refresh **first**, then stamp the error: the row exists
                // nest-side and belongs on the page (as non-revealable), but
                // `refresh` clears the page error on a clean read, so setting
                // it beforehand would silently erase the one signal telling the
                // user this secret will not be re-revealable.
                self.refresh().await;
                self.set_error(SAVE_LOCAL_ERROR_KEY, &e.to_string());
                self.observer.on_changed();
            }
        }
        Ok(secret)
    }

    /// Recover a credential's secret from the account's **own** credential
    /// store (`fauna.state.atproto`) so the user can reconfigure an app without
    /// revoking and re-minting. A client-side read — the nest is never
    /// consulted, because it structurally cannot answer (D3).
    ///
    /// Returns [`AtprotoSettingsError::SecretUnavailable`] when no local entry
    /// matches; clients should gate the affordance on
    /// [`AppCredentialRow::revealable`](crate::snapshots::AppCredentialRow::revealable)
    /// so that is a rare race rather than the normal path.
    pub async fn reveal_secret(
        &self,
        credential_id: String,
    ) -> Result<SecretString, AtprotoSettingsError> {
        let held = self
            .held_credentials()
            .await
            .map_err(|e| AtprotoSettingsError::Store {
                detail: e.to_string(),
            })?;
        held.iter()
            .find(|c| c.credential_id == credential_id)
            .map(|c| SecretString::new(String::from_utf8_lossy(c.secret.as_slice()).into_owned()))
            .ok_or(AtprotoSettingsError::SecretUnavailable { credential_id })
    }

    /// Revoke an app credential: nest first (which cascades to every session
    /// minted from it and nudges the bridge), then drop the local secret.
    ///
    /// A failure to drop the local copy is **not** escalated beyond the page
    /// error: the credential is already dead nest-side, so a lingering secret
    /// can never authenticate — it is inert bytes, unlisted (the nest no
    /// longer lists the row), and overwritten if the id is ever minted again.
    pub async fn revoke(&self, credential_id: String) {
        if let Err(e) = self
            .nest_api
            .revoke_app_credential(credential_id.clone())
            .await
        {
            self.set_error(REVOKE_ERROR_KEY, e.detail());
            self.observer.on_changed();
            return;
        }

        let local_error: Option<String> = match self.credential_store(false) {
            Ok(store) => store
                .revoke_app_credential(credential_id)
                .await
                .err()
                .map(|e| e.to_string()),
            Err(e) => Some(e.to_string()),
        };

        // Refresh before stamping, for the same reason as `mint`: a clean
        // `refresh` clears the page error, so an error set beforehand vanishes.
        self.refresh().await;
        if let Some(detail) = local_error {
            self.set_error(REVOKE_ERROR_KEY, &detail);
            self.observer.on_changed();
        }
    }

    /// Revoke one live session by its hex id (`atproto-connected-app-revoke`).
    /// Purely nest-side — a session holds no client-custodied material.
    pub async fn revoke_session(&self, session_id_hex: String) {
        let Ok(session_id) = hex::decode(&session_id_hex) else {
            // The id came out of a snapshot this machine hex-encoded, so a
            // malformed value is a client bug, not a user-reachable state.
            self.set_error(REVOKE_SESSION_ERROR_KEY, "malformed session id");
            self.observer.on_changed();
            return;
        };
        if let Err(e) = self.nest_api.revoke_session(session_id).await {
            self.set_error(REVOKE_SESSION_ERROR_KEY, e.detail());
            self.observer.on_changed();
            return;
        }
        self.refresh().await;
    }

    /// Flip the per-account external-apps kill-switch
    /// (`atproto-external-apps-enable`).
    ///
    /// Non-destructive in both directions: OFF suspends the whole external-app
    /// plane while keeping every credential and session row listed and
    /// individually revocable, and ON restores them. The destructive path is
    /// per-row [`Self::revoke`], never this switch — which is why the page
    /// keeps rendering the lists while it is off.
    pub async fn set_external_apps_enabled(&self, enabled: bool) {
        if let Err(e) = self.nest_api.set_external_apps_enabled(enabled).await {
            self.set_error(TOGGLE_ERROR_KEY, e.detail());
            self.observer.on_changed();
            return;
        }
        // Re-read rather than assume: the flag rides the credential listing, so
        // a refresh proves the nest actually applied it.
        self.refresh().await;
    }
}

// ── Identity-custody terminal gestures — deliberately NOT FFI-exported ───
//
// The act that uses the user's own senior rotation key (retiring the
// identity, a PLC tombstone) lives in shared Rust so seven apps cannot each
// re-derive an ordering that is a security invariant — and it stays OUT of
// the `uniffi::export` blocks until a shell actually calls it, because an
// exported-but-uncalled UniFFI surface is a **dark capability**
// (`ui/nests.md` § Trust facet), and `request_tombstone` is one of the most
// consequential capabilities this machine has: it rewrites the identity's
// public operation log. tui consumes this machine as **direct Rust** (no FFI
// hop), so the lead app can build the ceremony without an export; the export
// lands with the first FFI shell that renders the control, exactly as the
// D10 delegation gestures' export did.
//
// `request_tombstone` itself is now PRIVATE and will never be exported: since
// the `atproto-delete-tombstone` opt-in landed (2026-09-26) its one caller is
// `confirm_delete`, acting on the open card's tick
// (`set_delete_retire_identity`, exported with the macOS + iOS leg, the first
// FFI shell to render it). A
// shell therefore holds no entry point to the terminal act at all — only a
// tick on a ceremony card, which the machine refuses for an identity that
// cannot be retired.
//
// (`request_tombstone` sat inside the exported block until 2026-08-02 while its
// own doc comment said it did not — found while adding `request_contest`
// beside it, and corrected rather than copied. The recovery-fork contest
// gestures — `open_contest_confirm` / `cancel_contest` / `request_contest` —
// moved OUT of this block on 2026-08-22, the same day android became the
// first FFI shell rendering the ceremony. The delete-presence trio —
// `open_delete_confirm` / `cancel_delete` / `confirm_delete` — moved OUT on
// row 7's six-app trickle-down, android again the first FFI shell: see the
// exported block below the contest one's close. The retire opt-in's setter
// (2026-09-26) sat in a withheld block of its own after it until the macOS +
// iOS leg — the first FFI shell to render the toggle — moved it into that same
// exported block; `request_tombstone` stays here, private.)
impl AtprotoSettingsMachine {
    // Moved out of the exported block 2026-08-02, fixing a build break: it is a
    // PRIVATE helper returning a bare `ContestOutcome`, and `uniffi::export`
    // tries to lower every method in the block it decorates — so the enum was
    // asked for derives it should not have. Exactly the correction this block's
    // own header records for `request_tombstone`, one method later.
    /// [`Self::converge_contest`]'s body, plus whether the caller is a user
    /// gesture that is owed the outcome.
    ///
    /// The split exists because "quiet" is right for exactly one of the two
    /// callers. The **ambient** pass runs on every settings refresh and retries
    /// forever: an unreachable directory there is not a failure, it is a later
    /// pass, and painting an error for it would put a red line on a page the
    /// user merely opened. A pass the user **asked for** is the opposite — the
    /// same silence is a button that did nothing, which is what testing.md rule
    /// 11 forbids and what `ui/atproto.md` § User actions rules out by
    /// requiring the card to stay open with `error-message` populated.
    ///
    /// Returns `None` when there was nothing to converge (no alarm, no
    /// violation) and `Some` with what the directory did otherwise.
    async fn converge_contest_reporting(&self, report: bool) -> Option<ContestOutcome> {
        // Read, then release: every path below re-locks `state` (the `Mutex`
        // is not reentrant), the `else` arm included.
        let target = {
            let s = self.state.lock().unwrap();
            s.custody_did
                .as_ref()
                // Only did:plc has an operation log to fork; did:web custody is
                // domain custody, out of scope by the same rule the custody
                // check applies.
                .filter(|d| d.starts_with("did:plc:"))
                // …and only while a CONFIRMED custody alarm names this DID.
                // The contest exists to answer an alarm: with custody holding
                // there is nothing to plan, and planning anyway would make
                // every refresh of every healthy identity issue a second
                // request to the public PLC directory. [`Self::check_custody`]
                // runs immediately before this on the same convergence, so the
                // flag is always as fresh as the alarm itself.
                .filter(|d| s.custody_alarmed.as_deref() == Some(d.as_str()))
                .map(|d| {
                    (
                        d.clone(),
                        s.identity
                            .as_ref()
                            .map(|i| i.handle.clone())
                            .unwrap_or_default(),
                    )
                })
        };
        let Some((did, handle)) = target else {
            self.clear_contest();
            return None;
        };

        // The ring is both the comparison set for the plan and the only source
        // of a signature. Empty (another device minted and its rows have not
        // synced here) ⇒ this device can neither judge nor act; another can.
        let (ring, intents) = match self.identity_store().atproto_identity().await {
            Ok(custody) => (custody.rotation_keys, custody.contest_intents),
            Err(e) => return report.then_some(ContestOutcome::Refused(e)),
        };
        if ring.is_empty() {
            return report.then(|| {
                ContestOutcome::Refused(
                    "this device holds no recovery key for the identity".to_string(),
                )
            });
        }
        let held: Vec<String> = ring.iter().map(|k| k.pubkey_did_key.clone()).collect();
        let now = Timestamp::now_secs() as u64;

        let plan = match self.contest.plan(did.clone(), held, now).await {
            Ok(plan) => plan,
            Err(e) => {
                // Unreadable ≠ uncontested: quiet, retried next convergence,
                // exactly as the custody check treats the same directory.
                tracing::warn!(%did, %e, "could not plan a recovery-fork contest; will retry");
                return report.then(|| ContestOutcome::Refused(e.to_string()));
            }
        };
        let violation = match plan {
            ContestPlan::NoViolation => {
                self.clear_contest();
                return None;
            }
            ContestPlan::Violation(v) => v,
        };

        let card = contest_card(&violation, &handle, now);
        let already_consented = intents
            .iter()
            .any(|i| i.did == did && i.contested_op_cid == violation.contested_op_cid);
        {
            let mut s = self.state.lock().unwrap();
            s.contest = Some((violation.contested_op_cid.clone(), card));
        }
        self.observer.on_changed();

        if !already_consented {
            // The card is up; the next move is the user's. Decision 6.
            //
            // Reachable with `report` set only if the consent this gesture just
            // wrote no longer covers the violation the fresh read found — i.e.
            // a SECOND attack landed between the two, which decision 7 says
            // needs its own human decision rather than this one's consent.
            return report.then(|| {
                ContestOutcome::Refused(
                    "the identity changed again while you were deciding; check the new change"
                        .to_string(),
                )
            });
        }
        match self.contest.converge(did.clone(), ring, intents, now).await {
            Ok(ContestProgress::Contested {
                fork_prev_cid,
                signed_with_did_key,
            }) => {
                tracing::info!(
                    %did, %fork_prev_cid,
                    "recovery fork accepted; the contested operation and its descendants are nullified"
                );
                // The fork republishes this DID under the signing key, so the
                // published-for burn applies for the same reason the retire
                // path's does: a later mint reusing the key would publicly link
                // the identities. Quiet on failure — the custody check's
                // Verified arm re-records it on the very next pass.
                if !signed_with_did_key.is_empty()
                    && let Err(e) = record_published_binding(
                        &*self.identity_store(),
                        &did,
                        &signed_with_did_key,
                    )
                    .await
                {
                    tracing::warn!(%did, %e, "could not record the contest key's publication; a later custody pass will re-record it");
                }
                // Do NOT clear the card or the alarm here. Both come down off
                // the DIRECTORY's evidence on the next convergence — believing
                // our own submit would make the surface report a recovery the
                // network has not actually accepted.
                Some(ContestOutcome::Contested)
            }
            Ok(other) => {
                // Every remaining arm is a refusal that signed nothing. They
                // are expected states, not errors: no consent for THIS
                // violation, a window that closed, a genesis with no remedy.
                tracing::warn!(%did, ?other, "recovery-fork contest did not proceed");
                report.then(|| ContestOutcome::Refused(format!("{other:?}")))
            }
            Err(e) => {
                tracing::warn!(%did, %e, "recovery-fork contest failed; retry is safe");
                report.then(|| ContestOutcome::Refused(e.to_string()))
            }
        }
    }

    /// The delete-presence ceremony's "also permanently retire this identity"
    /// tick (`atproto-pds-bridge.md` § Disable & revocation layer 2).
    ///
    /// Two records, in an order that is a security invariant, not a style
    /// choice: the user's consent lands **client-side first**
    /// ([`record_tombstone_consent`] — the record the converge pass requires
    /// and the nest cannot author), and only then the
    /// durable intent nest-side. A crash between the two leaves
    /// consent-without-intent, which is inert (re-tick), never
    /// intent-without-consent, which [`Self::converge_tombstone`] would refuse
    /// forever. The same store-before-publish ordering the mint pins for the
    /// rotation key.
    ///
    /// Its one caller is [`Self::confirm_delete`], after the sweep landed,
    /// when the ceremony's `atproto-delete-tombstone` tick is set — so the
    /// terminal act has no entry point of its own, exported or not, and cannot
    /// be reached outside the ceremony even by a shell that wanted to. Returns
    /// the failure detail rather than painting it: the ceremony decides what
    /// the page shows (the card stays open for a retry).
    async fn request_tombstone(&self) -> Result<(), String> {
        let did = {
            let s = self.state.lock().unwrap();
            s.custody_did.clone()
        };
        let Some(did) = did.filter(|d| d.starts_with("did:plc:")) else {
            return Err("no retirable did:plc identity to opt in for".into());
        };
        record_tombstone_consent(&*self.identity_store(), &did)
            .await
            .map_err(|e| e.to_string())?;
        self.nest_api
            .request_tombstone()
            .await
            .map_err(|e| e.detail().to_string())?;
        Ok(())
    }

    /// Leave the ceremony open, re-enabled, with the page error populated —
    /// `ui/atproto.md` § User actions: "Failure keeps the card open with
    /// `error-message` populated; retry is safe."
    fn fail_ceremony(&self, detail: &str) {
        {
            let mut s = self.state.lock().unwrap();
            if let Some(c) = s.contest_confirm.as_mut() {
                c.in_progress = false;
            }
        }
        self.set_error(REQUEST_CONTEST_ERROR_KEY, detail);
        self.observer.on_changed();
    }
}

// ── The 72 h recovery-fork contest ceremony (`atproto-contest-*`) ────────
//
// EXPORTED 2026-08-22, on exactly the condition the block above withholds
// for: android is the first FFI shell rendering the ceremony (the
// linux/web/android trickle-down of `atproto-identity-custody.md` § The 72 h
// recovery-fork contest; tui already consumes this machine as direct Rust,
// so it needed no export). Scoped to EXACTLY the three contest gestures —
// `open_contest_confirm` / `cancel_contest` / `request_contest` — never
// `request_tombstone`, which is private (this block's header); the
// delete-presence ceremony has its own exported block below.
#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl AtprotoSettingsMachine {
    /// Open the contest ceremony (`atproto-contest`) — the confirm card that
    /// names the op, what this device would sign, and the deadline.
    ///
    /// Signs nothing and records no consent: it opens a surface, and
    /// [`Self::request_contest`] is the only thing on it that acts.
    ///
    /// **Refuses on a violation that cannot be fought**, loudly rather than by
    /// doing nothing (testing.md rule 11). A page cannot normally reach that —
    /// `atproto-contest` renders only at `ContestCardRow::show_contest` — which
    /// is exactly why the machine checks: decision 2's "no dead button on a
    /// hopeless state" is enforced once here rather than by seven pages each
    /// remembering to hide a control.
    pub fn open_contest_confirm(&self) {
        {
            let mut s = self.state.lock().unwrap();
            match s.contest.as_ref() {
                Some((_, card)) if card.show_contest => {
                    s.contest_confirm = Some(ContestConfirm { in_progress: false });
                }
                _ => {
                    drop(s);
                    self.set_error_localized(LocalizedText::key(CONTEST_NOT_CONTESTABLE_KEY));
                    self.observer.on_changed();
                    return;
                }
            }
        }
        self.observer.on_changed();
    }

    /// Close the ceremony without acting (`atproto-contest-cancel`): nothing
    /// signed, no intent recorded (`ui/atproto.md` § User actions). The
    /// violation notice above it stays up — the attack did not go away because
    /// the user is not ready to answer it.
    ///
    /// Inert while a submit is in flight, for [`Self::cancel_transition`]'s
    /// reason: the fork is already on the wire and the directory, not this
    /// card, decides what happens to it.
    pub fn cancel_contest(&self) {
        let mut s = self.state.lock().unwrap();
        if s.contest_confirm.as_ref().is_some_and(|c| c.in_progress) {
            return;
        }
        s.contest_confirm = None;
        drop(s);
        self.observer.on_changed();
    }

    /// The contest ceremony's confirm (`atproto-contest-confirm`): record the
    /// user's scoped consent to undo the box-authored operation the card names,
    /// then run the converge that performs it.
    ///
    /// **Takes no argument on purpose.** The op contested is the one the card
    /// the user just read was composed from, held machine-side — so nothing a
    /// client passes can redirect the gesture at a different operation, and the
    /// consent decision 6 scopes to a named op is scoped by construction rather
    /// than by every app remembering to pass the right CID.
    ///
    /// Consent lands **client-side first**, before anything is signed: the
    /// record is the authority the converge pass demands, and it is what lets a
    /// sibling device — or this one after a crash — finish the contest inside
    /// the window. A crash between the two leaves an unspent consent, which is
    /// inert if the violation is gone and completable if it is not.
    pub async fn request_contest(&self) {
        // The lock is taken and released before any `set_error` below:
        // `set_error_localized` re-locks `state`, and this `Mutex` is not
        // reentrant.
        let scoped = {
            let mut s = self.state.lock().unwrap();
            // Double-confirm guard, `confirm_transition`'s: the submit in
            // flight decides, and a second signature over the same violation
            // is a second fork we would then have to explain.
            if let Some(c) = s.contest_confirm.as_mut() {
                if c.in_progress {
                    return;
                }
                c.in_progress = true;
            }
            s.custody_did
                .clone()
                .zip(s.contest.as_ref().map(|(cid, _)| cid.clone()))
        };
        // Paint the progress state before the round-trip.
        self.observer.on_changed();
        // No card means no violation this client can see — and a consent
        // recorded against nothing would be the standing authorization
        // decision 6 forbids.
        let Some((did, contested_op_cid)) = scoped else {
            self.fail_ceremony("no contestable operation is standing on this identity");
            return;
        };
        let now = Timestamp::now_secs() as u64;
        if let Err(e) =
            record_contest_intent(&*self.identity_store(), &did, &contested_op_cid, now).await
        {
            self.fail_ceremony(&e.to_string());
            return;
        }
        // The converge re-reads the log and re-derives everything before it
        // signs, so this call is a prompt, never a shortcut past the checks.
        // `report = true`: this pass is a press, not the ambient sweep, so its
        // outcome is owed to the user who is waiting for it.
        match self.converge_contest_reporting(true).await {
            Some(ContestOutcome::Contested) => {
                // The user's part is done. The NOTICE stays until the directory
                // agrees (see `converge_contest_reporting`) — only the ceremony
                // closes.
                self.state.lock().unwrap().contest_confirm = None;
                self.observer.on_changed();
            }
            Some(ContestOutcome::Refused(detail)) => self.fail_ceremony(&detail),
            // The violation stopped standing under us — nothing failed, and the
            // convergence has already cleared both card and ceremony.
            None => self.observer.on_changed(),
        }
    }
}

// ── "Delete my Bluesky presence" (`ui/atproto.md` § User actions row 4) ──
//
// EXPORTED for row 7's six-app trickle-down, on exactly the condition the
// withholding comment above sets: android is the first FFI shell rendering
// the ceremony (linux/web/android all land it in this same batch; tui
// already consumes this machine as direct Rust, so it needed no export).
// The ceremony lives here, not in seven pages, for the contest ceremony's
// reason: it is the last surface before the page's one destructive call, and
// the copy it collects consent against must be the same copy everywhere.
// Three gestures — open, cancel, confirm — and only the third touches the
// wire. The fourth, the `atproto-delete-tombstone` tick
// (`set_delete_retire_identity`, the last method in this block), is a card
// mutation; the terminal act it opts into runs inside `confirm_delete`, after
// the sweep. It joined the block with the macOS + iOS leg, the first FFI shell
// to render the toggle.
#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl AtprotoSettingsMachine {
    /// Open the delete ceremony (`atproto-delete-presence`).
    ///
    /// Deletes nothing and calls nothing: it opens a surface, and
    /// [`Self::confirm_delete`] is the only thing on it that acts. It is
    /// deliberately its **own** card rather than the depth selector's — the
    /// delete is not one of the selector's transitions, and reusing that card
    /// would let a user reading a level-change description confirm a sweep.
    ///
    /// **Refuses loudly on a presence that is already gone** (testing.md rule
    /// 11), rather than opening a card whose confirm could only no-op. A page
    /// cannot normally reach that — `atproto-delete-presence` renders only at
    /// `show_delete_presence` — which is exactly why the machine checks: the
    /// rule is enforced once here rather than by seven pages each remembering
    /// to hide a control.
    pub fn open_delete_confirm(&self) {
        {
            let mut s = self.state.lock().unwrap();
            if s.identity.as_ref().is_some_and(presence_stands) {
                s.delete_confirm = Some(DeleteConfirm {
                    in_progress: false,
                    retire_identity: false,
                    swept: false,
                });
            } else {
                drop(s);
                self.set_error_localized(LocalizedText::key(DELETE_NOTHING_TO_DELETE_KEY));
                self.observer.on_changed();
                return;
            }
        }
        self.observer.on_changed();
    }

    /// Close the ceremony without acting (`atproto-delete-cancel`): nothing
    /// deleted, no wire call (`ui/atproto.md` § User actions).
    ///
    /// Inert while a confirm is in flight, for [`Self::cancel_transition`]'s
    /// reason: the sweep is already on the wire, and nest — not this card —
    /// decides what happens to it.
    pub fn cancel_delete(&self) {
        let mut s = self.state.lock().unwrap();
        if s.delete_confirm.as_ref().is_some_and(|d| d.in_progress) {
            return;
        }
        s.delete_confirm = None;
        drop(s);
        self.observer.on_changed();
    }

    /// Perform the sweep (`atproto-delete-confirm`) — the ONE wire call of the
    /// ceremony, `fauna.bridges.atproto.delete_presence`.
    ///
    /// **Takes no argument on purpose**, like the kind itself: every choice the
    /// flow offers is a choice about *whether* to proceed, which the card the
    /// user just read settles, so nothing a client passes can redirect the
    /// gesture at a different presence.
    ///
    /// On failure the card stays open with the page error populated and nothing
    /// changed; retry is safe (§ Errors & edge cases — the kind is idempotent).
    /// On success the state is re-read rather than assumed: nest's answer is
    /// the truth a racing client converges on, and the sweep itself is the
    /// bridge's own later pass — so the page must not claim the records are
    /// gone, only that the deletion started. The refresh takes the card down as
    /// a consequence of the presence no longer standing, not as a separate act.
    ///
    /// **With the `atproto-delete-tombstone` tick set**, the retirement opt-in
    /// is recorded after the sweep and only after it — consent client-side,
    /// then the intent nest-side ([`Self::request_tombstone`]) — and the
    /// refresh's converge pass publishes the tombstone once the sweep has
    /// finished. The order is the network's: nest refuses the opt-in for a
    /// presence not yet recorded deleted, and a DID retired before its sweep
    /// finished would strand the sweep's delete commits. A failure there keeps
    /// the card open, marked swept, for a retry that re-sends both steps.
    pub async fn confirm_delete(&self) {
        let retire = {
            let mut s = self.state.lock().unwrap();
            let Some(d) = s.delete_confirm.as_mut() else {
                return; // no open card: a double-press has nothing to act on
            };
            if d.in_progress {
                return; // the in-flight call decides
            }
            d.in_progress = true;
            d.retire_identity
        };
        self.observer.on_changed(); // paint the progress state

        // `newly_deleted == false` is a SUCCESS — a retry, or a second device
        // that got there first — so the outcome is deliberately not inspected
        // beyond the error arm. There is nothing different to tell a user whose
        // presence was destroyed a second ago by their other phone.
        if let Err(e) = self.nest_api.delete_presence().await {
            let mut s = self.state.lock().unwrap();
            if let Some(d) = s.delete_confirm.as_mut() {
                d.in_progress = false;
            }
            drop(s);
            self.set_error(DELETE_PRESENCE_ERROR_KEY, e.detail());
            self.observer.on_changed();
            return;
        }
        if retire && let Err(detail) = self.request_tombstone().await {
            {
                let mut s = self.state.lock().unwrap();
                if let Some(d) = s.delete_confirm.as_mut() {
                    d.in_progress = false;
                    d.swept = true;
                }
            }
            // Paint what the sweep changed, THEN the failure — a clean
            // refresh clears the page error, so the order is load-bearing.
            self.refresh().await;
            self.set_error(REQUEST_TOMBSTONE_ERROR_KEY, &detail);
            self.observer.on_changed();
            return;
        }
        self.state.lock().unwrap().delete_confirm = None;
        self.refresh().await;
    }

    /// The `atproto-delete-tombstone` opt-in: also permanently retire the
    /// identity once the sweep has finished (S5 slice 5b). A local mutation of
    /// the open card only — [`Self::confirm_delete`] is what acts on it.
    ///
    /// Exported with the macOS + iOS trickle-down leg, the first FFI shell to
    /// render the toggle (the dark-capability rule in the identity-custody
    /// block's header); tui consumes this machine as direct Rust.
    ///
    /// Inert without an open card and while a confirm is in flight (the card
    /// the user read is what they confirm). **Refuses loudly** to tick an
    /// identity that cannot be retired (testing.md rule 11): the checkbox
    /// renders greyed there, so a page reaching this misread its snapshot.
    pub fn set_delete_retire_identity(&self, enabled: bool) {
        let refused = {
            let mut s = self.state.lock().unwrap();
            let available = retire_unavailable_reason(&s).is_none();
            match s.delete_confirm.as_mut() {
                None => return,
                Some(d) if d.in_progress => return,
                Some(_) if enabled && !available => true,
                Some(d) => {
                    d.retire_identity = enabled;
                    false
                }
            }
        };
        if refused {
            self.set_error(
                REQUEST_TOMBSTONE_ERROR_KEY,
                "this identity cannot be permanently retired",
            );
        }
        self.observer.on_changed();
    }
}

// ── D10 delegated authoring (`atproto-pds-full.md` § D10) ────────────────
//
// EXPORTED 2026-07-31, on exactly the condition the withholding comment set.
// These were deliberately un-exported while tui — which consumes this machine as
// **direct Rust** (`ui/nests.md`'s tui bullet, no FFI hop) — was the only shell
// rendering the row, because an exported-but-uncalled UniFFI surface is a *dark
// capability* (`ui/nests.md` § Trust facet — generation recovery). The prior
// comment said "the export lands with the first shell that needs it"; apple is
// that shell (the D10 six-app trickle-down, `atproto-pds-full.md` § Problem 1 →
// D10 → Audit), so the export lands here and is called by
// `AtprotoSettingsView`'s delegation row on both apple apps. Still NOT exported
// to wasm: web has no delegation row yet, and adding one there would re-create
// the dark capability this comment exists to prevent.
#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl AtprotoSettingsMachine {
    /// Authorize external ATProto apps to post as this account: the whole D10
    /// mint ceremony in one gesture (§ D10 → Mint ceremony).
    ///
    /// Three nest calls in a fixed order — fetch `K_pub` (minting it), sign the
    /// cert over *that* key under the account's identity, upload it. The order
    /// is not a style choice: the nest's check 3 refuses a cert naming any key
    /// but the one it minted, so there is no shortcut past the fetch.
    ///
    /// The grant is **time-bounded by construction** — `DELEGATION_WINDOW_SECS`
    /// from now, a hard-coded constant with no chooser — per
    /// `principles.md`'s requirement that capability grants be time-bounded and
    /// the shipped grant shape in `ui/nests.md` § Expiry / renewal. Re-running
    /// this is also the *renewal* gesture: provisioning overwrites the stored
    /// cert with a freshly dated one, so a lapsed delegation recovers without a
    /// revoke first.
    pub async fn authorize_external_apps(&self) {
        // Checked BEFORE the fetch, which mints `K`: a client that cannot sign
        // must not leave a sub-key behind it will never authorize. An honest
        // refusal rather than a silent no-op, per testing.md rule 11.
        if self.identity.is_none() {
            self.set_error_localized(LocalizedText::key(NO_IDENTITY_ERROR_KEY));
            self.observer.on_changed();
            return;
        }

        let k_pub = match self.nest_api.fetch_authoring_key().await {
            Ok(k) => k,
            Err(e) => {
                self.set_error(AUTHORIZE_ERROR_KEY, e.detail());
                self.observer.on_changed();
                return;
            }
        };

        // MICROSECONDS on both sides: `Timestamp` is micros and the window
        // constant is seconds. The nest compares the same way (its check 5, and
        // the authoring-time wall-clock gate), and a millisecond clock here
        // would put every real expiry ~1000x into its own future — the exact
        // bug that made the nest's expiry check dead until 2026-07-29.
        let created_at = Timestamp::now();
        let expires_at = Timestamp(created_at.0 + DELEGATION_WINDOW_SECS * 1_000_000);

        // Signing is synchronous and the borrow is scoped to this block, so the
        // identity key is never held across an `await`.
        let minted = {
            let Some(identity) = self.identity.as_ref() else {
                self.set_error_localized(LocalizedText::key(NO_IDENTITY_ERROR_KEY));
                self.observer.on_changed();
                return;
            };
            build_authoring_delegation_cert(
                identity,
                k_pub,
                &AUTHORING_CAPABILITIES,
                created_at,
                Some(expires_at),
            )
        };
        let cert = match minted {
            Ok(c) => c,
            Err(e) => {
                self.set_error(AUTHORIZE_ERROR_KEY, &e.to_string());
                self.observer.on_changed();
                return;
            }
        };

        if let Err(e) = self.nest_api.provision_authoring_delegation(cert).await {
            self.set_error(AUTHORIZE_ERROR_KEY, e.detail());
            self.observer.on_changed();
            return;
        }

        // Re-read rather than assume, the same discipline the kill-switch
        // toggle uses: the row the page shows is the cert the nest really
        // stored, re-verified under this account's own key.
        self.refresh_delegation().await;
        self.observer.on_changed();
    }

    /// Revoke the authoring delegation (§ D10 → Revocation): the sub-key and
    /// its cert are destroyed nest-side, so external apps can no longer author.
    ///
    /// Already-published posts stay verifiable forever — their cert is embedded
    /// in their own wire — so this stops *future* authoring, not history. The
    /// user's tool for removing a published post is the ordinary tombstone.
    pub async fn deauthorize_external_apps(&self) {
        if let Err(e) = self.nest_api.revoke_authoring_delegation().await {
            self.set_error(DEAUTHORIZE_ERROR_KEY, e.detail());
            self.observer.on_changed();
            return;
        }
        self.refresh_delegation().await;
        self.observer.on_changed();
    }

    // ── The OAuth consent ceremony (F4 rung 2) ──────────────────────────

    /// Answer one pending consent request (`atproto-consent-approve` /
    /// `atproto-consent-deny`): `approved` decides which.
    ///
    /// This call — over the user's own authed WS-RPC connection — **is** the
    /// ceremony's trust root: the grant is Ed25519-rooted in the caller's
    /// identity, and the browser driving the OAuth flow never holds a Fauna
    /// secret. A decline is recorded rather than ignored, so the waiting browser
    /// gets a clean refusal instead of a timeout.
    ///
    /// Both outcomes re-list, and that is deliberate. Nest answers `false` when
    /// there was nothing live to resolve — already answered (possibly on the
    /// user's other device), expired, or not this caller's row — and decides all
    /// three inside the UPDATE's own `WHERE` clause, so they are one
    /// indistinguishable answer by construction. Guessing which would be
    /// inventing a distinction the wire deliberately does not draw; re-listing
    /// shows the user the truth instead. The page error says the request is no
    /// longer answerable, which is the honest common denominator.
    ///
    /// ⚠ There is **no expiry pre-check** here or in the card, and there must
    /// not be one: a request is answerable until nest says otherwise, a
    /// resolution is reported even past `expires_at`, and the snapshot row
    /// deliberately carries no timestamp to check against.
    pub async fn resolve_consent(&self, consent_id_hex: String, approved: bool) {
        let Ok(consent_id) = hex::decode(&consent_id_hex) else {
            // The id came out of a row this machine rendered, so a bad one is a
            // caller bug, not a user-reachable state — but it must still be a
            // loud refusal, never a dropped command (testing.md rule 11).
            self.set_error(
                CONSENT_ERROR_KEY,
                &format!("malformed consent id: {consent_id_hex}"),
            );
            self.observer.on_changed();
            return;
        };

        let row = self
            .state
            .lock()
            .unwrap()
            .consent_rows
            .iter()
            .find(|r| r.consent_id == consent_id)
            .cloned();
        // A row this machine never listed (the list moved under the card) can
        // still be declined — a decline mints nothing — but not approved: what
        // an approve grants is read off the row.
        let row = match row {
            Some(row) => row,
            None if !approved => NestConsentRow {
                consent_id,
                ..NestConsentRow::default()
            },
            None => {
                self.set_error_localized(LocalizedText::key(CONSENT_GONE_KEY));
                self.refresh_consents().await;
                self.observer.on_changed();
                return;
            }
        };
        let seams = self.consent_grants.lock().unwrap().clone();
        let nest = &self.nest_api;
        let answer = consent_grant::answer_consent(
            seams.as_deref(),
            &row,
            approved,
            |blob| nest.mint_grant(blob),
            |id, approved| nest.resolve_consent(id, approved),
            |grant_id| nest.revoke_grant(grant_id),
        )
        .await;
        match answer {
            Ok(true) => self.state.lock().unwrap().error = None,
            // No `message` argument: nest cannot say *which* of the three it
            // was, so neither can this line. A detail string here would be this
            // client inventing a distinction the wire deliberately refuses to
            // draw.
            Ok(false) => self.set_error_localized(LocalizedText::key(CONSENT_GONE_KEY)),
            Err(AnswerError::Nest(e)) => self.set_error(CONSENT_ERROR_KEY, e.detail()),
            Err(AnswerError::Grant(e)) => self.set_error(CONSENT_ERROR_KEY, &e.to_string()),
        }
        self.refresh_consents().await;
        self.observer.on_changed();
    }

    /// Re-list pending consent requests only — the poll a **visible** Bluesky
    /// page runs, deliberately lighter than `refresh()`'s full pass (no
    /// custody check, no tombstone/contest converge, none of which a
    /// backgrounded poll should repeat every tick).
    ///
    /// **Why a poll is needed at all.** An *unassigned* request (no
    /// `login_hint`) fans out to nobody by construction — see
    /// `refresh_consents`'s own doc — so a client that is already sitting on
    /// the page, with nothing else prompting a re-render, would otherwise
    /// never discover it. tui's nav model self-hydrates the whole page on
    /// every visit (`route_subpage`'s `SubPage::Bluesky` arm), which covers
    /// a fresh navigation; a native shell whose page identity survives a
    /// redundant re-navigation (apple's `.id(selectedSettingsPage)`) needs
    /// this as the backstop for staying on the page instead. Same shape as
    /// `resolve_consent`'s own tail.
    pub async fn poll_pending_consents(&self) {
        self.refresh_consents().await;
        self.observer.on_changed();
    }
}

// ── Internal helpers (not FFI-exported) ──────────────────────────────────
impl AtprotoSettingsMachine {
    /// Re-read the pending consent requests and rebuild their card rows. Split
    /// out of `refresh` for the same reason `refresh_delegation` is: answering
    /// one re-reads just this part, and the `consent_requested` push is a nudge
    /// to re-read exactly this. Does **not** notify — callers batch the paint.
    ///
    /// Degrades independently like every other read on this page: a failure
    /// keeps the rows already on screen (a stale card the user can still
    /// compare a code against beats a blank panel) and sets the page error.
    async fn refresh_consents(&self) {
        match self.nest_api.list_pending_consents().await {
            Ok(rows) => {
                let seams = self.consent_grants.lock().unwrap().clone();
                let folders = consent_grant::consent_folder_names(seams.as_deref(), &rows).await;
                let cards = rows
                    .iter()
                    .cloned()
                    .map(|r| consent_card_row(r, &folders))
                    .collect();
                let mut s = self.state.lock().unwrap();
                s.consents = cards;
                s.consent_rows = rows;
            }
            Err(e) => self.set_error(REFRESH_ERROR_KEY, e.detail()),
        }
    }

    /// Re-read the delegation and rebuild its row. Split out of `refresh` so
    /// the two gestures above can re-read just this part without a whole page
    /// round-trip. Does **not** notify — callers batch that into one paint.
    async fn refresh_delegation(&self) {
        let state = match self.nest_api.fetch_authoring_delegation().await {
            Ok(d) => d,
            Err(e) => {
                self.set_error(REFRESH_ERROR_KEY, e.detail());
                return;
            }
        };

        let last_used_at_millis = state.last_used_at;
        let Some(cert_bytes) = state.cert else {
            // No cert: either the ceremony never ran, or it was interrupted
            // after minting `K`. Both authorize nothing, so both render as
            // "not authorized" — a bare sub-key is not a delegation.
            self.state.lock().unwrap().delegation = None;
            return;
        };

        // The cert is verified under THIS account's identity key before it is
        // rendered. That is what makes the row a statement about what the user
        // signed rather than about what the nest chose to serve — and it is the
        // check that would catch a nest substituting a cert for a sub-key it
        // controls.
        let Some(identity) = self.identity.as_ref() else {
            // Without the identity key the cert cannot be verified. Fail safe:
            // render no row rather than an unverified one.
            self.state.lock().unwrap().delegation = None;
            return;
        };

        match parse_delegation_cert(&cert_bytes, &identity.actor_id()) {
            Ok(summary) => {
                let row = DelegationRow {
                    device_key_hex: hex::encode(summary.device_key),
                    capabilities: summary
                        .capabilities
                        .iter()
                        .map(|c| format!("{c:?}"))
                        .collect(),
                    authorized_at_micros: summary.created_at.0,
                    expires_at_micros: summary.expires_at.map(|t| t.0),
                    // `crate::delegation_clock::now()`, NOT `Timestamp::now()`:
                    // this is the one comparison a lapse e2e needs to fast-
                    // forward past the ~90-day window (convention 14 — a fake
                    // clock, never a sleep). The mint site above stays the
                    // real clock deliberately — see that module's docs.
                    liveness: delegation_liveness(
                        summary.expires_at,
                        crate::delegation_clock::now(),
                    )
                    .as_str()
                    .to_string(),
                    // Straight through from the wire — unlike every field above
                    // it, this one is NOT cert-derived and NOT verified. See the
                    // field's docs on why the page must frame it as advisory.
                    last_used_at_millis,
                };
                self.state.lock().unwrap().delegation = Some(row);
            }
            Err(e) => {
                // A stored cert this account did not sign is not a rendering
                // problem — it is a custody-grade mismatch. Surface it and show
                // no row, so nothing on screen implies an authorization the
                // user cannot be shown to have granted.
                self.state.lock().unwrap().delegation = None;
                self.set_error(DELEGATION_UNTRUSTED_KEY, &e.to_string());
            }
        }
    }

    /// Set the page error and log it once at the producer (observability.md §
    /// Log on the *event*, not the *paint* — the per-app views paint
    /// `snapshot().error` reactively on every observer tick).
    fn set_error(&self, key: &str, detail: &str) {
        self.set_error_localized(LocalizedText::key_arg(key, "message", detail.to_string()));
    }

    fn set_error_localized(&self, err: LocalizedText) {
        tracing::warn!(target: "fauna_atproto_settings", "{}", err.log_line());
        self.state.lock().unwrap().error = Some(err);
    }

    /// Execute the staged transition: gather the mint parameters, make the ONE
    /// nest call, converge on nest's reply. On failure the card stays open
    /// (`in_progress` drops back) with the page error set; the level is
    /// unchanged and retry is safe.
    async fn perform_staged_transition(&self) {
        let (target, needs_mint_params, history_backfill, did_method) = {
            let s = self.state.lock().unwrap();
            let Some(p) = s.pending.as_ref() else { return };
            (
                p.target,
                // Send the mint parameters whenever the move enters a hosted
                // level from outside — even when OUR view says "reactivate":
                // nest recomputes the plan from ITS state, and a stale client
                // whose identity view lags must not turn a genuine mint into
                // a malformed-parameters refusal. Nest ignores them on the
                // reactivate path.
                p.target.is_hosted() && !s.level.is_hosted(),
                s.history_backfill,
                s.did_method.clone(),
            )
        };

        // The rotation key is client-generated and client-custodied
        // (`fauna-client-atproto::rotation_key` — the sole custody
        // writer); only its did:key pubkey crosses the wire. `mint_rotation_key`
        // never returns a key already published for another DID: a re-mint
        // after a retirement sends a FRESH senior key, so the new identity's
        // public log shares nothing with the retired one's.
        let user_rotation_pub = if needs_mint_params && did_method == "plc" {
            match mint_rotation_key(&*self.identity_store(), Timestamp::now_secs() as u64).await {
                Ok(pubkey) => pubkey,
                Err(e) => {
                    if let Some(p) = self.state.lock().unwrap().pending.as_mut() {
                        p.in_progress = false;
                    }
                    self.set_error(TRANSITION_ERROR_KEY, &e.to_string());
                    self.observer.on_changed();
                    return;
                }
            }
        } else {
            String::new()
        };
        let (method_param, backfill_param) = if needs_mint_params {
            (did_method, history_backfill)
        } else {
            (String::new(), false)
        };

        match self
            .nest_api
            .set_integration_level(
                target.as_str().to_string(),
                method_param,
                user_rotation_pub,
                backfill_param,
            )
            .await
        {
            Ok(level) => {
                {
                    let mut s = self.state.lock().unwrap();
                    s.pending = None;
                    // Converge on nest's truth immediately; the refresh below
                    // re-reads everything else the transition changed.
                    if let Some(level) = IntegrationLevel::from_wire(&level) {
                        s.level = level;
                    }
                }
                self.refresh().await; // notifies
            }
            Err(e) => {
                if let Some(p) = self.state.lock().unwrap().pending.as_mut() {
                    p.in_progress = false;
                }
                self.set_error(TRANSITION_ERROR_KEY, e.detail());
                self.observer.on_changed();
            }
        }
    }
}

/// One grant composed for the connected-apps list, before it is known whether a
/// live session claims it.
///
/// Exists because the composition is **grant-primary for the OAuth half**: the
/// session pass takes the grants it can match, and whatever is left is the
/// suspended set. Carrying the id and the grant's own creation stamp alongside
/// the composed row is what lets a leftover become a row in its own right —
/// [`AtprotoGrantRow`] deliberately carries neither (one fact, one source, the
/// session being the usual source), and a suspended row has no session.
struct ComposedGrant {
    hex_id: String,
    suspended: bool,
    created_at_millis: i64,
    row: AtprotoGrantRow,
}

/// Compose one consent card from the seam's raw pending row — the ONE owner of
/// the card's composition (fence and wording alike), shared by this page and
/// the connected-apps page's Requests tray (`docs/goal/ui/connected-apps.md`
/// § Architectural rules: "the consent card here is the built card").
/// `folder_names` is the owner's own name for each folder a `folder:read`
/// scope names, as the folder seam resolved it
/// (`consent_grant::consent_folder_names`) — empty when it has none.
pub fn consent_card_row(
    c: NestConsentRow,
    folder_names: &std::collections::BTreeMap<i64, String>,
) -> ConsentCardRow {
    // A `records` scope is worded from what this device knows about the
    // request — the publisher, how many kinds the verified manifest
    // declares under it, and whether a writer key was attested
    // (`records_scope_row`); a `folder:read` scope names the folder when
    // the owner's custody resolves it (`fauna_scope::folder_read_card_row`);
    // every other scope through `describe_scope`.
    let scope_descriptions = c
        .scopes
        .iter()
        .map(|s| {
            records_scope_row(s, &c)
                .or_else(|| {
                    let id = fauna_bridge_atproto::fauna_scope::folder_read_qualifier(s)?;
                    fauna_bridge_atproto::fauna_scope::folder_read_card_row(
                        s,
                        folder_names.get(&id).map(String::as_str),
                    )
                })
                .unwrap_or_else(|| fauna_bridge_atproto::authz::describe_scope(s.clone()))
        })
        .map(|d| strip_control_chars(&d).into_owned())
        .collect();
    ConsentCardRow {
        consent_id_hex: hex::encode(&c.consent_id),
        code: c.code,
        client_id: c.client_id,
        // ⚠ **`client_name` is attacker-authored text, and it is
        // stripped of control characters HERE — at the one place
        // the card row is composed — never in a per-app painter**.
        //
        // The nest applies only `non_empty` to the value a client
        // publishes, so a name may carry newlines; every app paints
        // the card by interpolating this string into a structured
        // multi-line body and then splitting on `'\n'`, at which
        // point an attacker's newlines and the card's own
        // structural ones are the same byte — so the attacker owns
        // ROWS of the card, including a forged "It is asking to:"
        // heading above a scope list they wrote. The user compares
        // the binding code (which is unforgeable), it matches, and
        // they approve a grant described by the attacker.
        //
        // Stripping in the shared machine rather than in tui is
        // priority #2 and the whole point: six apps have yet to
        // paint this card, and a per-app fix leaves the trap armed
        // for all of them. A per-app *structural* fix (a dedicated
        // element for the name) is NOT sufficient either — a name
        // carrying 50 newlines still paints 50 rows.
        //
        // The tui sink-side control-character gate
        // cannot catch this and must not be moved to try: it runs
        // *after* the split that has already consumed newlines as
        // row structure, so by then the property is gone. It is
        // correct where it is; it is simply not this fence.
        client_name: c.client_name.map(|n| strip_control_chars(&n).into_owned()),
        // ONE owner of scope wording, shared with the browser
        // page the same user is looking at. Never a second
        // wording here — see `ConsentCardRow`. (A `records` row is that
        // owner's contextual form, `fauna_scope::records_card_row`.)
        //
        // Stripped in the same pass, and deliberately so even
        // though no scope token can carry a newline today
        // (`split_whitespace` upstream). The reason to strip
        // anyway is that the safety is a property of a *parser
        // two crates away*: describe_scope echoes raw attacker
        // text on its unknown-scope fallthrough, so the day that
        // splitter changes, this row would gain the same defect
        // silently. Cheap here, invisible if unnecessary.
        scope_descriptions,
        // The set grouping, fenced in this SAME pass — see
        // `consent_set_row`. `title`/`details` are authored by
        // whoever publishes the set's Lexicon record, which is
        // a different attacker from the one who wrote
        // `client_name` and exactly as unreviewed.
        sets: c.sets.into_iter().map(consent_set_row).collect(),
        ends: None,
    }
}

/// The card row of a `fauna:records:rw:` scope, worded by its one owner
/// (`fauna_scope::records_card_row`) from the request's verified manifest and
/// attested writer key — `None` for any other scope. A wildcard's kind count
/// is the manifest's declared kinds it covers; a manifest that does not verify
/// against the document's host leaves the count unknown (the approve refuses
/// it anyway).
fn records_scope_row(scope: &str, c: &NestConsentRow) -> Option<String> {
    let qualifier = fauna_bridge_atproto::fauna_scope::records_qualifier(scope)?;
    let kinds = match &qualifier {
        fauna_core::ext_kind::ExtQualifier::Kind(_) => Some(1),
        wildcard => c
            .fauna_manifest
            .as_deref()
            .zip(fauna_protocol::kind_manifest::client_id_host(&c.client_id))
            .and_then(|(jws, host)| fauna_protocol::kind_manifest::verify_manifest(jws, &host).ok())
            .map(|m| m.kinds.iter().filter(|k| wildcard.covers(&k.kind)).count()),
    };
    let writable = c.writer_ed25519.as_ref().is_some_and(|w| w.len() == 32);
    fauna_bridge_atproto::fauna_scope::records_card_row(scope, kinds, writable)
}

/// Word a space-delimited scope string through the one describe path, fenced
/// like every other scope list this crate composes. Shared with the
/// connected-apps roster so a grant reads the same on every surface.
pub fn scope_words(scopes: &str) -> Vec<String> {
    scopes
        .split_whitespace()
        .map(|s| fauna_bridge_atproto::authz::describe_scope(s.to_string()))
        .map(|d| strip_control_chars(&d).into_owned())
        .collect()
}

/// Compose the OAuth half of a connected-apps row from the wire grant.
///
/// Two rules, both of them the same rules the consent card composes under —
/// deliberately, because the two surfaces describe the *same* approval to the
/// same user at different times, and a divergence between "what you are being
/// asked for" and "what you approved" is exactly the doubt neither surface can
/// afford:
///
/// - **`describe_scope` is the one owner of scope wording** (shared with the
///   browser consent page). Never a second wording here or in a client.
/// - **Attacker text is stripped at composition** — `client_name` comes from a
///   document at a URL the requesting client chose, and every app paints this
///   row as one label, so a name carrying newlines would own rows in a list the
///   user reads as the nest's own structure. The scope descriptions are
///   stripped in the same pass for the reason the card documents: the safety
///   there is a property of a parser two crates away, cheap to re-assert here
///   and invisible if unnecessary.
fn grant_row(g: fauna_protocol::atproto_pds::AtprotoGrantInfo) -> AtprotoGrantRow {
    AtprotoGrantRow {
        client_id: g.client_id,
        client_name: g.client_name.map(|n| strip_control_chars(&n).into_owned()),
        scope_descriptions: scope_words(&g.scopes),
        // Composed by the SAME function the card uses, deliberately: the two
        // surfaces describe one approval at two moments, so a set that read
        // one way while being asked for and another way afterwards would be
        // the same divergence the scope wording is centralized to prevent.
        sets: g.sets.into_iter().map(grant_set).collect(),
        last_used_at_millis: g.last_used_at,
    }
}

/// The wire's grant set → the seam's shape → the card row, so the connected-apps
/// row and the consent card share one composition (fence and wording alike)
/// rather than growing a second one that can drift.
fn grant_set(s: fauna_protocol::atproto_pds::ConsentSetInfo) -> ConsentSetRow {
    consent_set_row(NestConsentSet {
        nsid: s.nsid,
        title: s.title,
        details: s.details,
        members: s.members,
    })
}

/// Compose one permission set's card row from the seam's raw set
/// (`atproto-pds-full.md:334`).
///
/// Three rules, and each one is a rule the flat scope list already lives under
/// — deliberately, because a set row is not a new kind of thing on this card,
/// it is the same approval with its grouping restored:
///
/// - **The NSID is the identity and crosses verbatim**, the way `client_id`
///   does. Nothing here derives a publisher, a display name or a fetch target
///   from it; the resolution that used it happened at PAR, once.
/// - **`title` and `details` are attacker text and are stripped HERE.** They
///   come from a Lexicon record published by whatever DID the NSID's authority
///   names — a *different* party from the client, and no more reviewed. Every
///   app paints them as labels beside the nest's own structure, so a title
///   carrying newlines would own rows in a list the user reads as ours.
/// - **`describe_scope` words the members**, the one owner shared with the flat
///   list and the browser page. A set's members are ordinary granular scopes
///   after expansion, so there is nothing here for a second vocabulary to
///   describe — and a set whose members read differently from the same scopes
///   listed flat would be the exact "is this the same request?" doubt the
///   binding code exists to remove.
fn consent_set_row(s: NestConsentSet) -> ConsentSetRow {
    ConsentSetRow {
        nsid: s.nsid,
        title: s
            .title
            .map(|t| strip_control_chars(t.as_str()).into_owned()),
        details: s
            .details
            .map(|d| strip_control_chars(d.as_str()).into_owned()),
        member_descriptions: s
            .members
            .into_iter()
            .map(fauna_bridge_atproto::authz::describe_scope)
            .map(|d| strip_control_chars(&d).into_owned())
            .collect(),
    }
}

/// Transcribe the seam's identity summary into the renderable row.
fn identity_row(i: NestIdentitySummary) -> IdentitySummaryRow {
    IdentitySummaryRow {
        handle: i.handle,
        method: i.method,
        status: i.status,
    }
}

/// Transcribe the seam's link summary into the renderable row.
fn link_row(l: LinkSummary) -> LinkSummaryRow {
    LinkSummaryRow { display: l.display }
}

/// Compose the transition card's copy from the shared [`TransitionPlan`] —
/// one localized line per effect, in the plan's own teardown-before-buildup
/// order. This is the client half of the single-source rule: nest executes
/// exactly this plan, so the card can promise exactly these effects
/// (`ui/atproto.md` § Transition semantics, card-copy column).
/// Compose the contest card from a plan the machine just re-derived off the
/// public directory's log (`ui/atproto.md` § Element IDs,
/// `atproto-contest-card`).
///
/// One composer, machine-side, for the same reason the transition card has
/// one: seven apps must not word an identity-compromise notice seven ways, and
/// a user comparing what two of their devices say about the same attack must
/// see the same sentence.
///
/// The countdown is **advisory** and is stated as such in the copy ("about N
/// hours left"): the directory rules on lateness when the fork is submitted, so
/// a precise-looking deadline would be a promise this side cannot keep.
fn contest_card(violation: &Violation, handle: &str, now_unix_secs: u64) -> ContestCardRow {
    let (state, detail_key, contestable) = match &violation.eligibility {
        ContestEligibility::Contestable(_) => (
            "contestable",
            "atproto_settings.contest_detail_contestable",
            true,
        ),
        ContestEligibility::WindowClosed => (
            "window-closed",
            "atproto_settings.contest_detail_window_closed",
            false,
        ),
        // Two structural no-remedy states, both rendering as `not-contestable`
        // per the ui.yaml scope (the state vocabulary is contestable /
        // window-closed / not-contestable; the *reason* is the detail line's
        // job, which is why a third state value would have been the wrong
        // shape here).
        ContestEligibility::GenesisViolation => (
            "not-contestable",
            "atproto_settings.contest_detail_genesis",
            false,
        ),
        // The log itself does not authenticate — a hostile directory or a MITM
        // serving a fabricated history, rather than a compromised box. Naming
        // it is what keeps the alarm from pointing at a blank page: the user
        // has already been told their identity may be seized, and this is the
        // surface that explains why nothing can be signed about it.
        ContestEligibility::Unauthenticated(_) => (
            "not-contestable",
            "atproto_settings.contest_detail_unauthenticated",
            false,
        ),
    };
    ContestCardRow {
        state: state.to_string(),
        detail: LocalizedText::key_arg(detail_key, "handle", handle.to_string()),
        // A countdown only where acting on it is possible: on a closed or
        // hopeless state it would read as a deadline the user might still meet.
        deadline: contestable
            .then_some(violation.deadline_unix)
            .flatten()
            .map(|deadline| {
                let left = deadline.saturating_sub(now_unix_secs);
                if left < 3600 {
                    LocalizedText::key("atproto_settings.contest_deadline_soon")
                } else {
                    LocalizedText::key_arg(
                        "atproto_settings.contest_deadline",
                        "hours",
                        (left / 3600).to_string(),
                    )
                }
            }),
        show_contest: contestable,
    }
}

/// Compose the ceremony's confirm card (`atproto-contest-confirm-card`) from
/// the notice it confirms.
///
/// `ui/atproto.md` § User actions requires this surface to name three things:
/// the op being contested, what the fork will sign, and the deadline. The first
/// two are its own copy; the third is the SAME machine-composed line the notice
/// above already carries, deliberately reused rather than recomputed — two
/// countdowns derived twice are two chances to disagree on the screen where a
/// user is deciding whether they still have time.
///
/// ⚠ The honesty rule of `contest_detail_contestable` binds here too: `contest_confirm_signs` names the key the nest keeps. Do not
/// drop that line to shorten the card.
fn compose_contest_confirm_lines(card: &ContestCardRow, s: &State) -> Vec<LocalizedText> {
    let handle = s
        .identity
        .as_ref()
        .map(|i| i.handle.clone())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| s.handle_preview.clone());
    let mut lines = vec![
        LocalizedText::key_arg(CONTEST_CONFIRM_UNDO_KEY, "handle", handle),
        LocalizedText::key(CONTEST_CONFIRM_SIGNS_KEY),
        LocalizedText::key(CONTEST_CONFIRM_DIRECTORY_KEY),
    ];
    lines.extend(card.deadline.clone());
    lines
}

/// Whether there is still a Bluesky presence to destroy.
///
/// `"deleted"` and `"tombstoned"` are the two terminal statuses — the sweep has
/// already run, and `delete_presence` against either is the idempotent no-op its
/// `newly_deleted: false` reports. Everything else (`"pending"`, `"active"`,
/// `"deactivated"`) still has a presence behind it, including a deactivated one:
/// the records are still served-and-then-unserved state that a sweep removes,
/// which is exactly why `ui/atproto.md` § User actions keeps the button
/// reachable after a step-down.
fn presence_stands(id: &IdentitySummaryRow) -> bool {
    id.status != "deleted" && id.status != "tombstoned"
}

/// The delete ceremony's card copy (`atproto-delete-confirm-card`).
///
/// Four lines, and each is load-bearing rather than decorative:
///
/// 1. **What the sweep destroys** — every projected record, through real
///    deletions the network is told about.
/// 2. **That the identity survives, named.** The single most important line on
///    the card: the sweep is "still reversible in identity terms"
///    (`atproto-pds-bridge.md` § Disable & revocation layer 2), so a ceremony
///    that read as "delete my account" would collect consent for something
///    stronger than what runs. Naming the handle is what makes the distinction
///    concrete instead of a reassurance.
/// 3. **That connected apps go down** — `delete_presence_handler` revokes every
///    session and destroys the D10 authoring credential unconditionally, so the
///    card says so rather than letting the user discover it afterwards.
/// 4. **Where the level lands** (`off`), and the honest caveat, reused verbatim
///    from the transition card.
///
/// **With the retire opt-in ticked, line 2 is replaced, not appended to:** the
/// terminal line names the identity that dies and says it cannot be undone.
/// A card carrying both would promise the identity survives beside the act
/// that destroys it — the blur between the reversible sweep and the terminal
/// retirement that would collect ticks from users who wanted the former.
fn compose_delete_confirm_lines(s: &State, retire: bool) -> Vec<LocalizedText> {
    let handle = s
        .identity
        .as_ref()
        .map(|i| i.handle.clone())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| s.handle_preview.clone());
    let identity_line = if retire {
        DELETE_CONFIRM_IDENTITY_RETIRED_KEY
    } else {
        DELETE_CONFIRM_IDENTITY_KEPT_KEY
    };
    vec![
        LocalizedText::key(DELETE_CONFIRM_SWEEP_KEY),
        LocalizedText::key_arg(identity_line, "handle", handle),
        LocalizedText::key(DELETE_CONFIRM_APPS_KEY),
        LocalizedText::key(DELETE_CONFIRM_LEVEL_OFF_KEY),
        LocalizedText::key(CARD_NO_RECALL_KEY),
    ]
}

/// Why this identity cannot be retired, or `None` when it can: a **published
/// did:plc** — the only kind with an operation log to tombstone. did:web's
/// custody IS domain custody (nest refuses the opt-in for it), and a did:plc
/// the bridge has not yet published has nothing in the directory to retire.
fn retire_unavailable_reason(s: &State) -> Option<LocalizedText> {
    if s.identity.as_ref().is_some_and(|i| i.method == "web") {
        return Some(LocalizedText::key(DELETE_RETIRE_UNAVAILABLE_WEB_KEY));
    }
    if s.custody_did
        .as_deref()
        .is_some_and(|d| d.starts_with("did:plc:"))
    {
        return None;
    }
    Some(LocalizedText::key(
        DELETE_RETIRE_UNAVAILABLE_UNPUBLISHED_KEY,
    ))
}

/// The open card's `atproto-delete-tombstone` row.
fn retire_opt_in(s: &State, open: &DeleteConfirm) -> RetireIdentityOptIn {
    let reason = retire_unavailable_reason(s);
    RetireIdentityOptIn {
        available: reason.is_none(),
        selected: open.retire_identity && reason.is_none(),
        unavailable_reason: reason,
    }
}

fn compose_card_lines(plan: &TransitionPlan, s: &State) -> Vec<LocalizedText> {
    let mut lines = Vec::new();
    if plan.unlinks_external {
        // "The link to {account} is removed; the external account itself is
        // untouched and is NOT migrated."
        let account = s
            .link
            .as_ref()
            .map(|l| l.display.clone())
            .unwrap_or_default();
        lines.push(LocalizedText::key_arg(CARD_UNLINK_KEY, "account", account));
    }
    if plan.mints_identity {
        lines.push(LocalizedText::key_arg(
            CARD_MINT_KEY,
            "handle",
            s.handle_preview.clone(),
        ));
    }
    if plan.reactivates_identity {
        let handle = s
            .identity
            .as_ref()
            .map(|i| i.handle.clone())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| s.handle_preview.clone());
        lines.push(LocalizedText::key_arg(
            CARD_REACTIVATE_KEY,
            "handle",
            handle,
        ));
    }
    if plan.mints_identity || plan.reactivates_identity {
        // The publishing-consent framing: public posts become visible on the
        // Bluesky network (projection starts/resumes).
        lines.push(LocalizedText::key(CARD_PUBLISH_CONSENT_KEY));
    }
    if plan.deactivates_identity {
        lines.push(LocalizedText::key(CARD_DEACTIVATE_KEY));
        lines.push(LocalizedText::key(CARD_NO_RECALL_KEY));
        lines.push(LocalizedText::key(CARD_DELETE_POINTER_KEY));
    }
    if plan.opens_login_plane {
        lines.push(LocalizedText::key(CARD_OPEN_PLANE_KEY));
        lines.push(LocalizedText::key(CARD_DM_HONESTY_KEY));
    }
    if plan.suspends_login_plane {
        lines.push(LocalizedText::key(CARD_SUSPEND_PLANE_KEY));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::FakeCredentialStore;
    use crate::nest_api::{
        AtprotoSettingsApiError, FakeAtprotoSettingsNestApi, FakeCall, NestCredentialRow,
    };
    use crate::observer::CountingObserver;
    use fauna_client_bridges::atproto_credential::verify_app_credential;

    fn nest_row(id: &str, dm_allowed: bool) -> NestCredentialRow {
        NestCredentialRow {
            credential_id: id.into(),
            label: id.into(),
            dm_allowed,
            created_at_millis: 1_700_000_000_000,
            last_used_at_millis: None,
        }
    }

    fn session_row(hex_id: &str, credential_id: Option<&str>) -> AtprotoSessionRow {
        AtprotoSessionRow {
            session_id_hex: hex_id.into(),
            plane: "app_credential".into(),
            credential_id: credential_id.map(Into::into),
            client_note: Some("Ivory on iPhone".into()),
            created_at_millis: 1_700_000_000_000,
            last_refreshed_at_millis: None,
            expires_at_millis: Some(1_800_000_000_000),
            // An app-credential session by default; `refresh` is what attaches
            // a grant, and the tests that care drive it through the fake.
            grant: None,
            suspended: false,
        }
    }

    /// The harness: the machine over the fake nest, with the credential store
    /// wired (the third element — every credential read and write lands there).
    fn machine() -> (
        Arc<AtprotoSettingsMachine>,
        Arc<FakeAtprotoSettingsNestApi>,
        FakeCredentialStore,
        Arc<CountingObserver>,
    ) {
        let api = Arc::new(FakeAtprotoSettingsNestApi::new());
        let credentials = FakeCredentialStore::new();
        let observer = CountingObserver::new();
        // Custody check inert by default: the quiet-failure verdict neither
        // posts alerts nor caches; the custody test module programs real ones.
        let verifier = crate::custody::FakeGenesisVerifier::new(Err(
            crate::custody::VerifyFailure::Fetch("test default: directory unreachable".into()),
        ));
        let m = AtprotoSettingsMachine::new(observer.clone(), api.clone(), verifier, None);
        m.set_credential_store(Arc::new(credentials.clone()));
        (m, api, credentials, observer)
    }

    /// The same harness with a real identity keypair wired, so the D10 mint
    /// ceremony can actually sign. The keypair is the account's own — the
    /// cert's `actor_id` is derived from it, which is what the nest's check 2
    /// and the client's own verify both bind to.
    fn machine_with_identity(
        identity: ActorKeypair,
    ) -> (
        Arc<AtprotoSettingsMachine>,
        Arc<FakeAtprotoSettingsNestApi>,
        Arc<CountingObserver>,
    ) {
        let api = Arc::new(FakeAtprotoSettingsNestApi::new());
        let observer = CountingObserver::new();
        let verifier = crate::custody::FakeGenesisVerifier::new(Err(
            crate::custody::VerifyFailure::Fetch("test default: directory unreachable".into()),
        ));
        let m = AtprotoSettingsMachine::new_with_identity(
            observer.clone(),
            api.clone(),
            verifier,
            None,
            Some(identity),
        );
        (m, api, observer)
    }

    // ── D10 delegated authoring ─────────────────────────────────────────

    #[tokio::test]
    async fn authorizing_runs_the_full_ceremony_in_order_and_renders_the_row() {
        let id = ActorKeypair::from_secret([11u8; 32]);
        let (m, api, _obs) = machine_with_identity(ActorKeypair::from_secret([11u8; 32]));
        api.set_minted_k_pub([0x2Bu8; 32]);

        m.authorize_external_apps().await;

        // The ceremony order is load-bearing: the cert must be signed over the
        // key the nest minted, so the fetch must precede the provision.
        let calls = api.calls();
        let fetch_at = calls
            .iter()
            .position(|c| matches!(c, FakeCall::FetchAuthoringKey))
            .expect("fetched the authoring key");
        let provision_at = calls
            .iter()
            .position(|c| matches!(c, FakeCall::ProvisionAuthoringDelegation { .. }))
            .expect("provisioned the cert");
        assert!(
            fetch_at < provision_at,
            "the cert must be signed over the minted key, so the fetch comes first: {calls:?}"
        );

        // What the nest ended up storing verifies under this account's own
        // identity key and names the key it minted — i.e. the ceremony really
        // produced a cert the nest's five checks accept.
        let stored = api.delegation().cert.expect("a cert was stored");
        let summary = parse_delegation_cert(&stored, &id.actor_id()).expect("verifies under us");
        assert_eq!(summary.device_key, [0x2Bu8; 32]);
        assert_eq!(summary.capabilities, AUTHORING_CAPABILITIES.to_vec());

        let row = m.snapshot().delegation.expect("the row renders");
        assert_eq!(row.device_key_hex, hex::encode([0x2Bu8; 32]));
        assert_eq!(row.liveness, "active");

        // Time-bounded by construction (principles.md): a freshly minted
        // delegation always carries an expiry, and it is the default window.
        let expires = row.expires_at_micros.expect("delegations are time-bounded");
        let window_micros = DELEGATION_WINDOW_SECS * 1_000_000;
        assert_eq!(expires - row.authorized_at_micros, window_micros);
    }

    #[tokio::test]
    async fn a_client_with_no_identity_key_refuses_loudly_and_mints_no_sub_key() {
        // The no-identity check must run BEFORE the fetch: minting `K` for a
        // client that can never authorize it would leave a sub-key behind.
        let (m, api, _cfg, _obs) = machine();

        m.authorize_external_apps().await;

        assert!(
            m.snapshot().error.is_some(),
            "a command this machine cannot honour must surface on error-message"
        );
        assert!(
            !api.calls()
                .iter()
                .any(|c| matches!(c, FakeCall::FetchAuthoringKey)),
            "no sub-key may be minted for a client that cannot sign a cert: {:?}",
            api.calls()
        );
        assert!(m.snapshot().delegation.is_none());
    }

    #[tokio::test]
    async fn a_bare_sub_key_with_no_cert_is_not_an_authorization() {
        // The interrupted-ceremony state: `K` exists, no cert. It authorizes
        // nothing, so it must not render as an authorization.
        let (m, api, _obs) = machine_with_identity(ActorKeypair::from_secret([12u8; 32]));
        api.set_delegation(crate::nest_api::AuthoringDelegationState {
            k_pub: Some([9u8; 32]),
            cert: None,
            ..Default::default()
        });

        m.refresh().await;

        assert!(m.snapshot().delegation.is_none());
    }

    #[tokio::test]
    async fn a_cert_this_account_did_not_sign_is_surfaced_not_rendered() {
        let (m, api, _obs) = machine_with_identity(ActorKeypair::from_secret([13u8; 32]));
        // A well-formed cert — signed by somebody else.
        let stranger = ActorKeypair::from_secret([99u8; 32]);
        let foreign = build_authoring_delegation_cert(
            &stranger,
            [9u8; 32],
            &AUTHORING_CAPABILITIES,
            Timestamp::now(),
            None,
        )
        .expect("mint");
        api.set_delegation(crate::nest_api::AuthoringDelegationState {
            k_pub: Some([9u8; 32]),
            cert: Some(foreign),
            ..Default::default()
        });

        m.refresh().await;

        let snap = m.snapshot();
        assert!(
            snap.delegation.is_none(),
            "nothing on screen may imply a grant this account cannot be shown to have made"
        );
        assert!(
            snap.error.is_some(),
            "the mismatch is surfaced, not swallowed"
        );
    }

    #[tokio::test]
    async fn revoking_clears_the_row_and_a_second_revoke_is_a_no_op_success() {
        let (m, _api, _obs) = machine_with_identity(ActorKeypair::from_secret([14u8; 32]));
        m.authorize_external_apps().await;
        assert!(m.snapshot().delegation.is_some());

        m.deauthorize_external_apps().await;
        assert!(m.snapshot().delegation.is_none());
        assert!(m.snapshot().error.is_none());

        // Idempotent: the desired end-state already holds, so revoking again
        // is a success with nothing to report.
        m.deauthorize_external_apps().await;
        assert!(m.snapshot().delegation.is_none());
        assert!(m.snapshot().error.is_none());
    }

    #[tokio::test]
    async fn re_authorizing_renews_a_lapsed_delegation_without_a_revoke_first() {
        let id = ActorKeypair::from_secret([15u8; 32]);
        let (m, api, _obs) = machine_with_identity(ActorKeypair::from_secret([15u8; 32]));
        // A delegation that lapsed long ago, as the nest would still hold it
        // (revoke is the only thing that deletes the row, and expiry is not
        // revocation).
        let long_ago = Timestamp(1_600_000_000_000_000);
        let lapsed = build_authoring_delegation_cert(
            &id,
            [0x3Cu8; 32],
            &AUTHORING_CAPABILITIES,
            long_ago,
            Some(Timestamp(long_ago.0 + 1_000_000)),
        )
        .expect("mint");
        api.set_delegation(crate::nest_api::AuthoringDelegationState {
            k_pub: Some([0x3Cu8; 32]),
            cert: Some(lapsed),
            ..Default::default()
        });

        m.refresh().await;
        assert_eq!(
            m.snapshot().delegation.expect("still shown").liveness,
            "expired",
            "a lapsed delegation stays visible so there is something to act on"
        );

        // Re-authorizing is the renewal gesture — no revoke needed.
        m.authorize_external_apps().await;
        let row = m.snapshot().delegation.expect("renewed");
        assert_eq!(row.liveness, "active");
        assert!(
            !api.calls()
                .iter()
                .any(|c| matches!(c, FakeCall::RevokeAuthoringDelegation)),
            "renewal must not require destroying the delegation first"
        );
    }

    #[tokio::test]
    async fn a_delegation_read_failure_does_not_blank_the_rest_of_the_page() {
        let (m, api, _obs) = machine_with_identity(ActorKeypair::from_secret([16u8; 32]));
        api.set_credentials(vec![nest_row("ivory", false)]);
        api.fail_fetch_delegation(AtprotoSettingsApiError::Transient {
            detail: "nest down".into(),
        });

        m.refresh().await;

        let snap = m.snapshot();
        assert_eq!(
            snap.credentials.len(),
            1,
            "the credential list is the half the user revokes with — it must survive"
        );
        assert!(snap.error.is_some());
    }

    // ── refresh ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn refresh_populates_credentials_sessions_and_kill_switch() {
        let (m, api, _cfg, observer) = machine();
        api.set_credentials(vec![nest_row("ivory", false), nest_row("graysky", true)]);
        api.set_sessions(vec![session_row("aabb", Some("ivory"))]);
        api.set_external_apps_enabled(false);

        m.refresh().await;

        let snap = m.snapshot();
        assert_eq!(snap.credentials.len(), 2);
        assert!(!snap.credentials[0].dm_allowed);
        assert!(snap.credentials[1].dm_allowed);
        assert_eq!(snap.sessions.len(), 1);
        assert!(!snap.external_apps_enabled);
        assert!(snap.error.is_none());
        assert_eq!(observer.count(), 1, "one tick for the whole page");
    }

    #[tokio::test]
    async fn kill_switch_defaults_on_before_any_fetch() {
        // A first paint must not render the switch as engaged: the nest column
        // defaults to 1, so an OFF-by-default snapshot would lie to the user.
        let (m, _api, _cfg, _o) = machine();
        assert!(m.snapshot().external_apps_enabled);
    }

    #[tokio::test]
    async fn refresh_marks_only_locally_held_credentials_revealable() {
        // The custody join: the nest lists both rows, but only `ivory` has a
        // secret in the account's credential store. `graysky` has none here
        // (minted before its secret could rest, or not yet synced) — listed,
        // not revealable, and that is a normal state.
        let (m, api, cfg, _o) = machine();
        api.set_credentials(vec![nest_row("ivory", false), nest_row("graysky", false)]);
        cfg.seed(AtprotoAppCredential {
            credential_id: "ivory".into(),
            label: "Ivory".into(),
            secret: SecretByteBuf::new(b"abcd-efgh-ijkl-mnop".to_vec()),
            dm_allowed: false,
            created_at: 1_700_000_000,
        });

        m.refresh().await;

        let snap = m.snapshot();
        assert!(snap.credentials[0].revealable, "local secret ⇒ revealable");
        assert!(
            !snap.credentials[1].revealable,
            "sibling-device credential is listed but not revealable here"
        );
    }

    #[tokio::test]
    async fn refresh_failure_keeps_prior_data_and_sets_error() {
        let (m, api, _cfg, _o) = machine();
        api.set_credentials(vec![nest_row("ivory", false)]);
        m.refresh().await;

        api.fail_list(AtprotoSettingsApiError::Transient {
            detail: "boom".into(),
        });
        m.refresh().await;

        let snap = m.snapshot();
        assert_eq!(snap.credentials.len(), 1, "prior data kept on failure");
        assert!(snap.error.is_some());
    }

    #[tokio::test]
    async fn a_sessions_failure_does_not_blank_the_credential_list() {
        // The credential list is the half the user revokes with; a sessions
        // fetch fault must not take it off screen.
        let (m, api, _cfg, _o) = machine();
        api.set_credentials(vec![nest_row("ivory", false)]);
        api.fail_list_sessions(AtprotoSettingsApiError::Transient {
            detail: "sessions down".into(),
        });

        m.refresh().await;

        let snap = m.snapshot();
        assert_eq!(snap.credentials.len(), 1, "credentials still rendered");
        assert!(snap.sessions.is_empty());
        assert!(snap.error.is_some(), "the fault is still surfaced");
    }

    #[tokio::test]
    async fn a_credential_store_read_failure_hides_the_reveal_affordance_rather_than_promising_it()
    {
        let (m, api, cfg, _o) = machine();
        api.set_credentials(vec![nest_row("ivory", false)]);
        cfg.set_load_failure(true);

        m.refresh().await;

        let snap = m.snapshot();
        assert_eq!(snap.credentials.len(), 1);
        assert!(
            !snap.credentials[0].revealable,
            "unprovable ⇒ hide, never offer a reveal that would then error"
        );
        assert!(
            snap.error.is_none(),
            "an absent account runtime (a web tab that hosts none) is a normal \
             state, not a page error: {:?}",
            snap.error
        );
    }

    // ── mint ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn mint_sends_a_verifier_and_never_the_secret_then_persists_locally() {
        let (m, api, cfg, _o) = machine();

        let secret = m.mint("Ivory".into(), true).await.expect("mint succeeds");

        let calls = api.calls();
        let provision = calls
            .iter()
            .find_map(|c| match c {
                FakeCall::ProvisionAppCredential {
                    credential_id,
                    label,
                    verifier,
                    dm_allowed,
                } => Some((credential_id, label, verifier, dm_allowed)),
                _ => None,
            })
            .expect("provision was called");
        assert_eq!(provision.0, "ivory", "kebab-derived id");
        assert_eq!(provision.1, "Ivory", "label verbatim");
        assert!(*provision.3);
        assert!(
            provision.2.starts_with("$argon2id$"),
            "the nest receives a PHC verifier, got {:?}",
            provision.2
        );
        assert!(
            !provision.2.contains(secret.as_str()),
            "D3: the secret must never cross the nest seam"
        );

        // The recoverable copy landed in the credential store, and the verifier the nest
        // holds really does verify that secret (the cross-language format the
        // Go bridge checks at createSession).
        let stored = cfg.current();
        assert_eq!(stored.app_credentials.len(), 1);
        let row = &stored.app_credentials[0];
        assert_eq!(row.credential_id, "ivory");
        assert_eq!(row.secret.as_slice(), secret.as_str().as_bytes());
        assert!(row.dm_allowed);
        assert!(verify_app_credential(secret.as_str(), provision.2));
    }

    #[tokio::test]
    async fn mint_derives_a_unique_id_against_both_the_nest_and_the_credential_store() {
        // `ivory` exists only nest-side (minted on a sibling device). Deriving
        // from the local config alone would mint a colliding id.
        let (m, api, _cfg, _o) = machine();
        api.set_credentials(vec![nest_row("ivory", false)]);
        m.refresh().await;

        m.mint("Ivory".into(), false).await.expect("mint succeeds");

        let minted = api
            .calls()
            .into_iter()
            .find_map(|c| match c {
                FakeCall::ProvisionAppCredential { credential_id, .. } => Some(credential_id),
                _ => None,
            })
            .expect("provision was called");
        assert_eq!(minted, "ivory-2", "collision-suffixed against the nest row");
    }

    #[tokio::test]
    async fn mint_nest_failure_leaves_no_orphan_secret_in_the_credential_store() {
        // The whole reason the nest call comes first: a failed provision must
        // not leave secret material for a credential that does not exist.
        let (m, api, cfg, _o) = machine();
        api.fail_provision(AtprotoSettingsApiError::Transient {
            detail: "nest refused".into(),
        });

        let err = m.mint("Ivory".into(), false).await.expect_err("mint fails");

        assert!(matches!(err, AtprotoSettingsError::Nest { .. }));
        assert!(
            cfg.current().app_credentials.is_empty(),
            "nothing persisted locally when the nest never stored the verifier"
        );
        assert!(m.snapshot().error.is_some());
    }

    /// An unreadable store only narrows the collision set to the nest's
    /// listing (every LIVE credential): the mint goes ahead and its secret
    /// still rests once the store takes the write.
    #[tokio::test]
    async fn an_unreadable_credential_store_still_mints_against_the_nest_listing() {
        let (m, api, cfg, _o) = machine();
        api.set_credentials(vec![nest_row("ivory", false)]);
        m.refresh().await;
        cfg.set_load_failure(true);

        let secret = m.mint("Ivory".into(), false).await.expect("minted");

        let stored = cfg.current();
        assert_eq!(stored.app_credentials.len(), 1);
        assert_eq!(
            stored.app_credentials[0].credential_id, "ivory-2",
            "collision-suffixed against the nest listing alone"
        );
        assert_eq!(
            stored.app_credentials[0].secret.as_slice(),
            secret.as_str().as_bytes()
        );
    }

    /// No credential store stands behind the seam: an unwired machine hands
    /// the minted secret back once and flags that it was not kept — the
    /// partial-success contract — never a silent success.
    #[tokio::test]
    async fn an_unwired_credential_store_flags_the_minted_secret_as_not_kept() {
        let api = Arc::new(FakeAtprotoSettingsNestApi::new());
        let unwired = AtprotoSettingsMachine::new(
            CountingObserver::new(),
            api.clone(),
            crate::custody::FakeGenesisVerifier::new(Err(crate::custody::VerifyFailure::Fetch(
                "test default".into(),
            ))),
            None,
        );

        let secret = unwired
            .mint("Ivory".into(), false)
            .await
            .expect("handed back");

        assert!(!secret.as_str().is_empty());
        let snap = unwired.snapshot();
        assert!(snap.error.is_some(), "the unkept secret must be surfaced");
        assert!(snap.credentials.iter().all(|c| !c.revealable));
        assert!(matches!(
            unwired.reveal_secret("ivory".into()).await,
            Err(AtprotoSettingsError::Store { .. })
        ));
    }

    /// A revoked id is free again (the nest deletes its row, the store
    /// tombstones its own), so the same label re-mints the same id, and the
    /// fresh secret is the one revealed.
    #[tokio::test]
    async fn a_revoked_id_is_minted_again_with_its_fresh_secret() {
        let (m, _api, cfg, _o) = machine();
        let first = m.mint("Ivory".into(), false).await.unwrap();
        m.revoke("ivory".into()).await;
        assert!(cfg.current().app_credentials.is_empty());

        let second = m.mint("Ivory".into(), false).await.unwrap();

        assert_ne!(first.as_str(), second.as_str());
        let revealed = m.reveal_secret("ivory".into()).await.expect("revealed");
        assert_eq!(revealed.as_str(), second.as_str());
    }

    #[tokio::test]
    async fn mint_returns_the_secret_even_when_the_local_save_fails_and_flags_it() {
        // The credential is already live nest-side, so swallowing the secret
        // would strand a working credential nobody can use or re-reveal.
        let (m, _api, cfg, _o) = machine();
        cfg.arm_save_failure();

        let secret = m
            .mint("Ivory".into(), false)
            .await
            .expect("the secret still comes back");

        assert!(!secret.as_str().is_empty());
        assert!(
            cfg.current().app_credentials.is_empty(),
            "the save really did fail"
        );
        let snap = m.snapshot();
        assert!(
            snap.error.is_some(),
            "the partial success MUST be surfaced, not swallowed by the refresh"
        );
        assert_eq!(
            snap.credentials.len(),
            1,
            "the live nest row is still listed"
        );
        assert!(
            !snap.credentials[0].revealable,
            "and correctly marked non-revealable"
        );
    }

    #[tokio::test]
    async fn minted_secrets_are_distinct_per_mint() {
        let (m, _api, _cfg, _o) = machine();
        let a = m.mint("One".into(), false).await.unwrap();
        let b = m.mint("Two".into(), false).await.unwrap();
        assert_ne!(a.as_str(), b.as_str());
    }

    // ── reveal ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn reveal_returns_the_minted_secret_from_the_credential_store() {
        let (m, _api, _cfg, _o) = machine();
        let minted = m.mint("Ivory".into(), false).await.unwrap();

        let revealed = m.reveal_secret("ivory".into()).await.expect("revealed");

        assert_eq!(revealed.as_str(), minted.as_str());
    }

    #[tokio::test]
    async fn reveal_of_a_sibling_device_credential_is_secret_unavailable() {
        let (m, api, _cfg, _o) = machine();
        api.set_credentials(vec![nest_row("graysky", false)]);
        m.refresh().await;

        let err = m
            .reveal_secret("graysky".into())
            .await
            .expect_err("no local secret");

        assert!(matches!(
            err,
            AtprotoSettingsError::SecretUnavailable { ref credential_id } if credential_id == "graysky"
        ));
    }

    // ── revoke ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn revoke_calls_the_nest_then_drops_the_local_secret() {
        let (m, api, cfg, _o) = machine();
        m.mint("Ivory".into(), false).await.unwrap();
        assert_eq!(cfg.current().app_credentials.len(), 1);

        m.revoke("ivory".into()).await;

        assert!(
            api.calls().contains(&FakeCall::RevokeAppCredential {
                credential_id: "ivory".into()
            }),
            "the nest is told first"
        );
        assert!(
            cfg.current().app_credentials.is_empty(),
            "local secret dropped"
        );
        assert!(m.snapshot().credentials.is_empty());
        assert!(m.snapshot().error.is_none());
    }

    #[tokio::test]
    async fn revoke_nest_failure_keeps_the_local_secret() {
        // If the nest refused, the credential is still live — dropping the
        // local copy would strand a working credential with no re-reveal path.
        let (m, api, cfg, _o) = machine();
        m.mint("Ivory".into(), false).await.unwrap();
        api.fail_revoke(AtprotoSettingsApiError::Transient {
            detail: "nope".into(),
        });

        m.revoke("ivory".into()).await;

        assert_eq!(
            cfg.current().app_credentials.len(),
            1,
            "secret kept — the credential still exists"
        );
        assert!(m.snapshot().error.is_some());
    }

    #[tokio::test]
    async fn revoke_surfaces_a_local_drop_failure_without_losing_the_refresh() {
        let (m, _api, cfg, _o) = machine();
        m.mint("Ivory".into(), false).await.unwrap();
        cfg.arm_save_failure();

        m.revoke("ivory".into()).await;

        let snap = m.snapshot();
        assert!(snap.credentials.is_empty(), "the nest row really is gone");
        assert!(
            snap.error.is_some(),
            "the local-drop failure survives the refresh that follows it"
        );
    }

    #[tokio::test]
    async fn revoking_a_credential_cascades_to_its_sessions() {
        let (m, api, _cfg, _o) = machine();
        api.set_credentials(vec![nest_row("ivory", false)]);
        api.set_sessions(vec![
            session_row("aa", Some("ivory")),
            session_row("bb", Some("graysky")),
        ]);
        m.refresh().await;
        assert_eq!(m.snapshot().sessions.len(), 2);

        m.revoke("ivory".into()).await;

        let snap = m.snapshot();
        assert_eq!(snap.sessions.len(), 1, "only the sibling session survives");
        assert_eq!(snap.sessions[0].credential_id.as_deref(), Some("graysky"));
    }

    // ── connected-apps: the grant ⋈ session join ────────────────────────

    fn grant(
        hex_id: &str,
        client_name: Option<&str>,
        scopes: &str,
    ) -> fauna_protocol::atproto_pds::AtprotoGrantInfo {
        fauna_protocol::atproto_pds::AtprotoGrantInfo {
            grant_id: hex::decode(hex_id).unwrap(),
            client_id: "https://ivory.app/client-metadata.json".into(),
            client_name: client_name.map(Into::into),
            scopes: scopes.into(),
            created_at: 1_700_000_000_000,
            last_used_at: Some(1_750_000_000_000),
            expires_at: None,
            ..Default::default()
        }
    }

    /// The join key is not a heuristic: the nest writes the grant row and its
    /// session family with the SAME identifier in one transaction, so an OAuth
    /// session finds its grant by id and an app-credential session finds none.
    #[tokio::test]
    async fn a_grant_joins_onto_the_session_that_shares_its_id() {
        let (m, api, _cfg, _o) = machine();
        api.set_sessions(vec![
            session_row("aabb", None),
            session_row("ccdd", Some("ivory")),
        ]);
        api.set_grants(vec![grant(
            "aabb",
            Some("Ivory"),
            "atproto repo:app.bsky.feed.post",
        )]);

        m.refresh().await;

        let snap = m.snapshot();
        let joined = &snap.sessions[0];
        let g = joined
            .grant
            .as_ref()
            .expect("the oauth row carries its grant");
        assert_eq!(g.client_name.as_deref(), Some("Ivory"));
        assert_eq!(g.client_id, "https://ivory.app/client-metadata.json");
        assert_eq!(g.last_used_at_millis, Some(1_750_000_000_000));
        assert!(
            snap.sessions[1].grant.is_none(),
            "an app-credential session has no grant behind it, so the richer \
             half must be structurally absent rather than empty-but-present"
        );
    }

    /// A **suspended** grant still reaches the connected-apps surface, even
    /// though its paired session is dead and therefore absent from
    /// `list_sessions`.
    ///
    /// `ui/atproto.md`'s downward matrix (`:111`) is explicit that a step-down
    /// (or `delete_presence`'s teardown) leaves grant rows "kept, listed,
    /// individually revocable", and the nest half derives exactly that
    /// (`list_atproto_oauth_grants` reports `suspended` for a grant whose
    /// same-id session is revoked — `atproto-oauth-provider.md:127`). A
    /// session-primary join drops it on the floor: the composition iterated
    /// live sessions and looked each one's grant up, so a grant with no live
    /// session was never visited at all and the nest's fix stayed inert.
    ///
    /// Grant-primary for the OAuth half is what makes the two halves agree.
    #[tokio::test]
    async fn a_suspended_grant_is_listed_even_though_its_session_is_gone() {
        let (m, api, _cfg, _o) = machine();
        // The step-down state exactly: the session row is revoked (so
        // `list_sessions` no longer returns it) while the grant row stays at
        // rest, flagged suspended.
        api.set_sessions(vec![]);
        let mut g = grant("aabb", Some("Ivory"), "atproto repo:app.bsky.feed.post");
        g.suspended = true;
        api.set_grants(vec![g]);

        m.refresh().await;

        let snap = m.snapshot();
        assert_eq!(
            snap.sessions.len(),
            1,
            "a suspended grant must still be listed — the matrix says rows are \
             KEPT and individually revocable, and a dropped row is neither"
        );
        let row = &snap.sessions[0];
        assert!(row.suspended, "and it must read as suspended, not as live");
        assert_eq!(
            row.session_id_hex, "aabb",
            "keyed by the grant id, which IS the session-family id revoke_session takes"
        );
        assert_eq!(row.plane, "oauth");
        assert_eq!(
            row.expires_at_millis, None,
            "a suspended row has no live refresh horizon — rendering one would \
             promise a working connection until that date"
        );
        assert_eq!(
            row.grant
                .as_ref()
                .expect("its identity half")
                .client_name
                .as_deref(),
            Some("Ivory"),
        );
    }

    /// A live grant and a suspended one sit in ONE list, each distinguishable —
    /// the surface the user revokes from must not split into two vocabularies.
    #[tokio::test]
    async fn live_and_suspended_grants_are_listed_together_and_told_apart() {
        let (m, api, _cfg, _o) = machine();
        let mut live_session = session_row("aabb", None);
        live_session.plane = "oauth".into();
        api.set_sessions(vec![live_session, session_row("ccdd", Some("ivory"))]);
        let mut dead = grant("eeff", Some("Graysky"), "atproto");
        dead.suspended = true;
        api.set_grants(vec![
            grant("aabb", Some("Ivory"), "atproto repo:app.bsky.feed.post"),
            dead,
        ]);

        m.refresh().await;

        let snap = m.snapshot();
        assert_eq!(
            snap.sessions.len(),
            3,
            "two oauth rows plus the credential row"
        );
        let by_id = |id: &str| {
            snap.sessions
                .iter()
                .find(|s| s.session_id_hex == id)
                .unwrap_or_else(|| panic!("row {id} must be listed"))
        };
        assert!(!by_id("aabb").suspended, "the live oauth row");
        assert_eq!(
            by_id("aabb").expires_at_millis,
            Some(1_800_000_000_000),
            "a live row keeps its session's real refresh horizon"
        );
        assert!(by_id("eeff").suspended, "the suspended oauth row");
        assert!(
            !by_id("ccdd").suspended,
            "an app-credential session is never suspended by this mechanism — \
             its plane has no grant row at all"
        );
    }

    /// A grant made from a permission set carries that provenance onto the
    /// connected-apps row — the **same** composition the consent card used, not
    /// a second one.
    ///
    /// The two surfaces describe one approval at two moments: the card when the
    /// user decides, this row a month later when they audit. A set that read
    /// one way while being asked for and another way afterwards is the same
    /// divergence the scope wording is centralized to prevent — so the pin
    /// asserts the fence holds here too (a title with a newline), which is only
    /// true because `grant_set` delegates to `consent_set_row` rather than
    /// transcribing the wire a second time.
    #[tokio::test]
    async fn a_grant_carries_its_permission_sets_composed_like_the_card() {
        let (m, api, _cfg, _o) = machine();
        let mut g = grant("aabb", Some("Ivory"), "atproto repo:com.example.event");
        g.sets = vec![fauna_protocol::atproto_pds::ConsentSetInfo {
            nsid: "com.example.calendar.appPerms".into(),
            title: Some("Calendar\nsync".into()),
            details: None,
            members: vec!["repo:com.example.event".into()],
            extra: Default::default(),
        }];
        api.set_grants(vec![g]);
        api.set_sessions(vec![session_row("aabb", None)]);

        m.refresh().await;

        let row = &m.snapshot().sessions[0];
        let set = &row.grant.as_ref().unwrap().sets[0];
        assert_eq!(set.nsid, "com.example.calendar.appPerms");
        assert_eq!(
            set.title.as_deref(),
            Some("Calendarsync"),
            "the set author's text is fenced HERE, by the card's own composition"
        );
        assert_eq!(set.member_descriptions.len(), 1);
        assert_ne!(
            set.member_descriptions[0], "repo:com.example.event",
            "members are worded by describe_scope on this surface too"
        );
    }

    /// Scope wording has ONE owner, shared with the consent card and the
    /// browser page. A user comparing what they approved against what this row
    /// says they approved must not be reading two vocabularies.
    #[tokio::test]
    async fn the_row_renders_describe_scopes_wording_not_the_raw_scope() {
        let (m, api, _cfg, _o) = machine();
        api.set_sessions(vec![session_row("aabb", None)]);
        api.set_grants(vec![grant("aabb", None, "atproto repo:app.bsky.feed.post")]);

        m.refresh().await;

        let snap = m.snapshot();
        let g = snap.sessions[0].grant.as_ref().unwrap();
        let expected: Vec<String> = ["atproto", "repo:app.bsky.feed.post"]
            .iter()
            .map(|s| fauna_bridge_atproto::authz::describe_scope(s.to_string()))
            .collect();
        assert_eq!(g.scope_descriptions, expected);
    }

    /// The label-injection class, on the surface that inherited it. `client_name`
    /// comes from a document at a URL the *requesting client* chose, and every
    /// app paints this row as one label — so newlines in the name would let an
    /// attacker own rows in a list the user reads as the nest's own structure.
    /// The fence is here, at composition, because that is the only place it
    /// holds for all seven apps at once.
    #[tokio::test]
    async fn an_attackers_newlines_in_the_client_name_cannot_forge_rows() {
        let (m, api, _cfg, _o) = machine();
        api.set_sessions(vec![session_row("aabb", None)]);
        api.set_grants(vec![grant(
            "aabb",
            Some("Ivory\n  full account access\nApproved"),
            "atproto",
        )]);

        m.refresh().await;

        let snap = m.snapshot();
        let name = snap.sessions[0]
            .grant
            .as_ref()
            .unwrap()
            .client_name
            .clone()
            .unwrap();
        assert!(
            !name.contains('\n'),
            "a newline here paints as a row break in every app: {name:?}"
        );
    }

    /// Security finding: the same attack one field over, and the one
    /// that was actually SHIPPED. On the OAuth plane `client_note` is the same
    /// attacker string as `client_name` — the nest's one-transaction write
    /// copies the resolved name into both tables — so stripping the grant half
    /// alone closed nothing, because the painter reads this field. The proof
    /// that made it urgent: forged newlines produced the connected-apps list's
    /// OWN "No connected apps" empty state, above a live grant.
    #[tokio::test]
    async fn an_attackers_newlines_in_the_client_note_cannot_forge_rows() {
        let (m, api, _cfg, _o) = machine();
        let mut hostile = session_row("aabb", None);
        hostile.plane = "oauth".into();
        hostile.client_note = Some("Ivory — created\nNo connected apps\n".into());
        api.set_sessions(vec![hostile]);

        m.refresh().await;

        let note = m.snapshot().sessions[0].client_note.clone().unwrap();
        assert!(
            !note.contains('\n'),
            "a newline here paints as a row break — and the forged row is the \
             list's own empty state: {note:?}"
        );
    }

    /// The degradation direction is deliberate: a grants failure must leave the
    /// sessions rendered *without* their richer half, never blank the list —
    /// that list is the affordance the user revokes with, and taking it away
    /// exactly when something is wrong is the wrong failure mode.
    #[tokio::test]
    async fn a_grants_failure_keeps_the_sessions_and_reports_the_error() {
        let (m, api, _cfg, _o) = machine();
        api.set_sessions(vec![session_row("aabb", None)]);
        api.set_grants(vec![grant("aabb", Some("Ivory"), "atproto")]);
        api.fail_list_grants(AtprotoSettingsApiError::Transient {
            detail: "nest unreachable".into(),
        });

        m.refresh().await;

        let snap = m.snapshot();
        assert_eq!(snap.sessions.len(), 1, "the revocable row survives");
        assert!(snap.sessions[0].grant.is_none());
        assert!(snap.error.is_some(), "and the failure is reported");
    }

    // ── sessions ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn revoke_session_removes_the_row() {
        let (m, api, _cfg, _o) = machine();
        api.set_sessions(vec![session_row("aabb", Some("ivory"))]);
        m.refresh().await;

        m.revoke_session("aabb".into()).await;

        assert!(m.snapshot().sessions.is_empty());
        assert!(api.calls().contains(&FakeCall::RevokeSession {
            session_id: vec![0xaa, 0xbb]
        }));
    }

    #[tokio::test]
    async fn revoke_session_with_a_malformed_id_errors_without_calling_the_nest() {
        let (m, api, _cfg, _o) = machine();

        m.revoke_session("not-hex".into()).await;

        assert!(m.snapshot().error.is_some());
        assert!(
            !api.calls()
                .iter()
                .any(|c| matches!(c, FakeCall::RevokeSession { .. })),
            "a malformed id never reaches the wire"
        );
    }

    // ── kill switch ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn kill_switch_flip_round_trips_through_a_refresh() {
        let (m, api, _cfg, _o) = machine();
        m.refresh().await;
        assert!(m.snapshot().external_apps_enabled);

        m.set_external_apps_enabled(false).await;

        assert!(
            api.calls()
                .contains(&FakeCall::SetExternalAppsEnabled { enabled: false })
        );
        assert!(
            !m.snapshot().external_apps_enabled,
            "the flag is re-read from the nest, not assumed"
        );
    }

    #[tokio::test]
    async fn kill_switch_off_keeps_every_row_listed_and_revocable() {
        // The switch is non-destructive by design: rows stay visible and
        // individually revocable while the plane is suspended. A client that
        // hid the lists when it is off would remove the user's only revoke
        // affordance.
        let (m, api, _cfg, _o) = machine();
        api.set_credentials(vec![nest_row("ivory", false)]);
        api.set_sessions(vec![session_row("aa", Some("ivory"))]);
        m.refresh().await;

        m.set_external_apps_enabled(false).await;

        let snap = m.snapshot();
        assert!(!snap.external_apps_enabled);
        assert_eq!(snap.credentials.len(), 1, "credential rows kept");
        assert_eq!(snap.sessions.len(), 1, "session rows kept");

        m.revoke("ivory".into()).await;
        assert!(
            m.snapshot().credentials.is_empty(),
            "and still revocable while off"
        );
    }

    #[tokio::test]
    async fn kill_switch_failure_sets_the_error_and_leaves_the_prior_flag() {
        let (m, api, _cfg, _o) = machine();
        m.refresh().await;
        api.fail_set_enabled(AtprotoSettingsApiError::Transient {
            detail: "nope".into(),
        });

        m.set_external_apps_enabled(false).await;

        let snap = m.snapshot();
        assert!(snap.external_apps_enabled, "unchanged — the nest refused");
        assert!(snap.error.is_some());
    }

    // ── custody guard ───────────────────────────────────────────────────

    #[tokio::test]
    async fn the_rendered_snapshot_never_carries_the_secret() {
        // Guards the `snapshots.rs` rule that the passively-rendered state is
        // secret-free: a snapshot is cloned into every observing view on every
        // tick, far beyond where a zeroizing type can discipline it.
        //
        // Scope: this catches the realistic regression — someone adding a
        // plain `String`/`Vec<u8>` secret field to the snapshot. It would NOT
        // catch a field typed `SecretBytes`, whose `Debug` is redacted by
        // design; that shape is instead prevented by review of `snapshots.rs`,
        // whose module doc states the rule.
        let (m, _api, _cfg, _o) = machine();
        let secret = m.mint("Ivory".into(), false).await.unwrap();

        let rendered = format!("{:?}", m.snapshot());

        assert!(
            !rendered.contains(secret.as_str()),
            "the secret leaked into the rendered snapshot: {rendered}"
        );
        assert!(
            rendered.contains("ivory"),
            "sanity: the row itself IS in the snapshot"
        );
    }
}

#[cfg(test)]
mod selector_tests {
    use super::*;
    use crate::nest_api::{
        AtprotoSettingsApiError, FakeAtprotoSettingsNestApi, FakeCall, LinkSummary,
        NestIdentitySummary,
    };
    use crate::observer::CountingObserver;
    use fauna_client_atproto::identity_store::InMemoryAtprotoIdentityStore;

    fn machine() -> (
        Arc<AtprotoSettingsMachine>,
        Arc<FakeAtprotoSettingsNestApi>,
        InMemoryAtprotoIdentityStore,
        Arc<CountingObserver>,
    ) {
        let api = Arc::new(FakeAtprotoSettingsNestApi::new());
        let custody = InMemoryAtprotoIdentityStore::default();
        let observer = CountingObserver::new();
        // Custody check inert by default: the quiet-failure verdict neither
        // posts alerts nor caches; the custody test module programs real ones.
        let verifier = crate::custody::FakeGenesisVerifier::new(Err(
            crate::custody::VerifyFailure::Fetch("test default: directory unreachable".into()),
        ));
        let m = AtprotoSettingsMachine::new(observer.clone(), api.clone(), verifier, None);
        m.set_identity_store(Arc::new(custody.clone()));
        m.set_credential_store(Arc::new(crate::credentials::FakeCredentialStore::new()));
        (m, api, custody, observer)
    }

    fn set_level_calls(api: &FakeAtprotoSettingsNestApi) -> Vec<FakeCall> {
        api.calls()
            .into_iter()
            .filter(|c| matches!(c, FakeCall::SetIntegrationLevel { .. }))
            .collect()
    }

    // ── refresh populates the selector state ────────────────────────────

    #[tokio::test]
    async fn refresh_populates_level_gate_preview_identity_and_link() {
        let (m, api, _cfg, _o) = machine();
        api.set_status(
            "hosted_visible",
            true,
            "example.com",
            "alice.example.com",
            Some(NestIdentitySummary {
                handle: "alice.example.com".into(),
                method: "plc".into(),
                status: "active".into(),
                did: None,
                ..Default::default()
            }),
        );

        m.refresh().await;

        let snap = m.snapshot();
        assert_eq!(snap.level, "hosted_visible");
        assert!(snap.hosted_allowed);
        assert!(snap.hosted_gate_reason.is_none());
        assert_eq!(snap.handle_preview, "alice.example.com");
        let id = snap.identity.expect("identity rendered");
        assert_eq!(id.status, "active");
        assert!(snap.link.is_none());
        assert!(
            !snap.show_did_method_radio,
            "after mint the method is a fact, displayed not chosen"
        );
        assert!(snap.show_delete_presence);
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn a_gated_box_greys_with_a_reason_naming_the_domain() {
        let (m, api, _cfg, _o) = machine();
        api.set_status("off", false, "localhost", "", None);

        m.refresh().await;

        let snap = m.snapshot();
        assert!(!snap.hosted_allowed);
        let reason = snap.hosted_gate_reason.expect("greyed WITH a reason");
        assert!(
            format!("{reason:?}").contains("localhost"),
            "the reason names the domain: {reason:?}"
        );
    }

    #[tokio::test]
    async fn a_deactivated_identity_stays_visible_at_level_off() {
        // `ui/atproto.md` § Errors & edge cases: the user must see what
        // re-enabling would restore, and the delete action stays reachable.
        let (m, api, _cfg, _o) = machine();
        api.set_status(
            "off",
            true,
            "example.com",
            "alice.example.com",
            Some(NestIdentitySummary {
                handle: "alice.example.com".into(),
                method: "plc".into(),
                status: "deactivated".into(),
                did: None,
                ..Default::default()
            }),
        );

        m.refresh().await;

        let snap = m.snapshot();
        assert_eq!(snap.level, "off");
        assert_eq!(snap.identity.unwrap().status, "deactivated");
        assert!(snap.show_delete_presence);
    }

    // ── "Delete my Bluesky presence": the confirm ceremony ───────────────
    //
    // `ui/atproto.md` § User actions rows 4/`atproto-delete-confirm`/`-cancel`:
    // the button opens its OWN card (never the depth selector's), the confirm
    // is the one wire call, and the cancel deletes nothing. The ceremony lives
    // machine-side for the same reason the contest ceremony does — seven apps
    // must run one ceremony, not seven.

    /// A hosted identity to delete: `active`, at `hosted_visible`, with a live
    /// external-app session so the teardown has something to tear down.
    async fn with_hosted_identity() -> (
        Arc<AtprotoSettingsMachine>,
        Arc<FakeAtprotoSettingsNestApi>,
        InMemoryAtprotoIdentityStore,
        Arc<CountingObserver>,
    ) {
        let (m, api, cfg, o) = machine();
        api.set_status(
            "hosted_visible",
            true,
            "example.com",
            "alice.example.com",
            Some(NestIdentitySummary {
                handle: "alice.example.com".into(),
                method: "plc".into(),
                status: "active".into(),
                did: Some("did:plc:abc".into()),
                ..Default::default()
            }),
        );
        m.refresh().await;
        (m, api, cfg, o)
    }

    fn delete_calls(api: &FakeAtprotoSettingsNestApi) -> usize {
        api.calls()
            .iter()
            .filter(|c| matches!(c, FakeCall::DeletePresence))
            .count()
    }

    #[tokio::test]
    async fn opening_the_delete_card_deletes_nothing_and_is_not_the_depth_card() {
        let (m, api, _cfg, _o) = with_hosted_identity().await;
        assert!(
            m.snapshot().delete_confirm.is_none(),
            "the ceremony is closed until the user opens it"
        );

        m.open_delete_confirm();

        let snap = m.snapshot();
        assert!(snap.delete_confirm.is_some(), "the card opens");
        assert!(
            snap.pending_transition.is_none(),
            "§ User actions: the delete ceremony has its OWN card, distinct \
             from the depth selector's transition card"
        );
        assert_eq!(delete_calls(&api), 0, "opening a card is not a mutation");
        assert_eq!(snap.level, "hosted_visible", "nothing changed yet");
    }

    #[tokio::test]
    async fn the_delete_card_names_the_sweep_the_surviving_identity_and_the_caveat() {
        let (m, _api, _cfg, _o) = with_hosted_identity().await;
        m.open_delete_confirm();

        let card = m.snapshot().delete_confirm.expect("the card opens");
        let keys: Vec<&str> = card.lines.iter().map(|l| l.key.as_str()).collect();
        assert!(
            keys.contains(&"atproto_settings.delete_confirm_sweep"),
            "what is destroyed: {keys:?}"
        );
        // The framing rule (`atproto-pds-bridge.md` § Disable & revocation
        // layer 2): the sweep is "still reversible in identity terms", so the
        // card must NOT read as "delete my account".
        assert!(
            keys.contains(&"atproto_settings.delete_confirm_identity_kept"),
            "the identity survives: {keys:?}"
        );
        // The honest caveat, reusing the transition card's own line rather
        // than wording a second one — two copies of a promise are two chances
        // to disagree (the `contest_deadline` precedent).
        assert!(
            keys.contains(&"atproto_settings.card_no_recall"),
            "the honest caveat: {keys:?}"
        );
        assert!(
            keys.contains(&"atproto_settings.delete_confirm_level_off"),
            "the level lands on off: {keys:?}"
        );
        let named = card
            .lines
            .iter()
            .any(|l| format!("{l:?}").contains("alice.example.com"));
        assert!(named, "the surviving identity is named: {card:?}");
        assert!(!card.in_progress);
    }

    #[tokio::test]
    async fn cancelling_the_delete_card_closes_it_and_deletes_nothing() {
        let (m, api, _cfg, _o) = with_hosted_identity().await;
        m.open_delete_confirm();

        m.cancel_delete();

        let snap = m.snapshot();
        assert!(snap.delete_confirm.is_none(), "the card closes");
        assert_eq!(delete_calls(&api), 0, "§ User actions: nothing deleted");
        assert_eq!(snap.level, "hosted_visible");
        assert!(snap.error.is_none(), "cancelling is not a failure");
    }

    #[tokio::test]
    async fn confirming_sweeps_the_presence_and_lands_the_level_on_off() {
        let (m, api, _cfg, _o) = with_hosted_identity().await;
        m.open_delete_confirm();

        m.confirm_delete().await;

        assert_eq!(delete_calls(&api), 1, "exactly one wire call");
        let snap = m.snapshot();
        assert!(snap.delete_confirm.is_none(), "the ceremony is over");
        // § User actions: "Confirming lands the level on `off`" — a selector
        // parked on a hosted rung would offer to restore something and then not.
        assert_eq!(snap.level, "off");
        assert_eq!(
            snap.identity.as_ref().map(|i| i.status.as_str()),
            Some("deleted"),
            "the identity survives the sweep, marked deleted"
        );
        assert!(
            snap.sessions.is_empty(),
            "the login plane went down with the presence"
        );
        assert!(snap.error.is_none());
    }

    // ── The "also permanently retire this identity" opt-in (S5 slice 5b) ─
    //
    // `atproto-pds-bridge.md` § Disable & revocation layer 2: the PLC
    // tombstone is reachable ONLY as an explicit, never-pre-ticked opt-in
    // inside this ceremony, and it runs strictly after the sweep.

    fn request_tombstone_calls(api: &FakeAtprotoSettingsNestApi) -> usize {
        api.calls()
            .iter()
            .filter(|c| matches!(c, FakeCall::RequestTombstone))
            .count()
    }

    async fn consents(cfg: &InMemoryAtprotoIdentityStore) -> Vec<String> {
        cfg.current().tombstone_consents
    }

    #[tokio::test]
    async fn the_retire_opt_in_is_offered_unticked_for_a_plc_identity() {
        let (m, _api, _cfg, _o) = with_hosted_identity().await;
        m.open_delete_confirm();

        let opt = m.snapshot().delete_confirm.expect("card").retire_identity;
        assert!(opt.available, "a published did:plc identity can be retired");
        assert!(!opt.selected, "never pre-ticked (§ Don't do these)");
        assert!(opt.unavailable_reason.is_none());
    }

    #[tokio::test]
    async fn ticking_retire_replaces_the_identity_survives_line_with_the_terminal_one() {
        let (m, _api, _cfg, _o) = with_hosted_identity().await;
        m.open_delete_confirm();

        m.set_delete_retire_identity(true);

        let card = m.snapshot().delete_confirm.expect("card");
        assert!(card.retire_identity.selected);
        let keys: Vec<&str> = card.lines.iter().map(|l| l.key.as_str()).collect();
        // The card must never promise the identity survives while the tick
        // that destroys it is set — the two lines are mutually exclusive.
        assert!(
            !keys.contains(&"atproto_settings.delete_confirm_identity_kept"),
            "a ticked card must not say the identity is kept: {keys:?}"
        );
        assert!(
            keys.contains(&"atproto_settings.delete_confirm_identity_retired"),
            "the terminal line names what dies: {keys:?}"
        );
        assert!(
            card.lines
                .iter()
                .any(|l| format!("{l:?}").contains("alice.example.com")),
            "the retired identity is named: {card:?}"
        );

        m.set_delete_retire_identity(false);
        let keys: Vec<String> = m
            .snapshot()
            .delete_confirm
            .expect("card")
            .lines
            .iter()
            .map(|l| l.key.clone())
            .collect();
        assert!(keys.contains(&"atproto_settings.delete_confirm_identity_kept".to_string()));
    }

    #[tokio::test]
    async fn the_tick_does_not_survive_the_card_being_closed() {
        let (m, _api, _cfg, _o) = with_hosted_identity().await;
        m.open_delete_confirm();
        m.set_delete_retire_identity(true);
        m.cancel_delete();

        m.open_delete_confirm();

        let opt = m.snapshot().delete_confirm.expect("card").retire_identity;
        assert!(!opt.selected, "a re-opened ceremony starts unticked");
    }

    #[tokio::test]
    async fn the_tick_does_nothing_without_an_open_card() {
        let (m, api, cfg, _o) = with_hosted_identity().await;
        m.set_delete_retire_identity(true);
        m.open_delete_confirm();

        assert!(
            !m.snapshot()
                .delete_confirm
                .expect("card")
                .retire_identity
                .selected
        );
        m.confirm_delete().await;
        assert_eq!(request_tombstone_calls(&api), 0);
        assert!(consents(&cfg).await.is_empty());
    }

    #[tokio::test]
    async fn confirming_without_the_tick_never_asks_for_retirement() {
        let (m, api, cfg, _o) = with_hosted_identity().await;
        m.open_delete_confirm();

        m.confirm_delete().await;

        assert_eq!(delete_calls(&api), 1);
        assert_eq!(request_tombstone_calls(&api), 0, "the tick is opt-in");
        assert!(consents(&cfg).await.is_empty(), "no consent recorded");
    }

    #[tokio::test]
    async fn confirming_with_the_tick_sweeps_first_then_consent_then_intent() {
        let (m, api, cfg, _o) = with_hosted_identity().await;
        m.open_delete_confirm();
        m.set_delete_retire_identity(true);

        m.confirm_delete().await;

        // The wire order is the invariant: nest refuses the opt-in for any
        // identity not already recorded deleted, and a tombstoned DID stops
        // resolving, which would strand the sweep's delete commits.
        let order: Vec<&str> = api
            .calls()
            .iter()
            .filter_map(|c| match c {
                FakeCall::DeletePresence => Some("delete"),
                FakeCall::RequestTombstone => Some("tombstone"),
                _ => None,
            })
            .collect();
        assert_eq!(order, vec!["delete", "tombstone"]);
        assert_eq!(
            consents(&cfg).await,
            vec!["did:plc:abc".to_string()],
            "the client-side consent the converge pass requires"
        );
        let snap = m.snapshot();
        assert!(snap.delete_confirm.is_none(), "the ceremony is over");
        assert_eq!(snap.identity.expect("identity").status, "deleted");
        assert!(snap.error.is_none(), "{:?}", snap.error);
    }

    #[tokio::test]
    async fn a_failed_retire_step_keeps_the_card_open_and_a_retry_completes_it() {
        let (m, api, cfg, _o) = with_hosted_identity().await;
        m.open_delete_confirm();
        m.set_delete_retire_identity(true);
        api.fail_request_tombstone(AtprotoSettingsApiError::Transient {
            detail: "transport.disconnected".into(),
        });

        m.confirm_delete().await;

        // The sweep landed, the retirement did not. The presence no longer
        // stands, but the card must not vanish with it: the user asked for a
        // retirement, and closing the card here would leave no way to finish
        // it (the button withdraws once the presence is deleted).
        let snap = m.snapshot();
        assert_eq!(snap.identity.as_ref().expect("identity").status, "deleted");
        let card = snap
            .delete_confirm
            .expect("the card stays open for a retry");
        assert!(!card.in_progress, "re-enabled");
        assert!(
            card.retire_identity.selected,
            "the tick is kept for the retry"
        );
        assert!(snap.error.is_some(), "the failure is on the page");

        api.clear_request_tombstone_failure();
        m.confirm_delete().await;

        assert_eq!(
            delete_calls(&api),
            2,
            "the retry re-sends the idempotent sweep"
        );
        assert_eq!(request_tombstone_calls(&api), 2);
        assert_eq!(consents(&cfg).await, vec!["did:plc:abc".to_string()]);
        let snap = m.snapshot();
        assert!(snap.delete_confirm.is_none(), "the ceremony is over");
        assert!(snap.error.is_none(), "{:?}", snap.error);
    }

    #[tokio::test]
    async fn a_did_web_identity_gets_a_greyed_opt_in_that_cannot_be_ticked() {
        let (m, api, _cfg, _o) = machine();
        api.set_status(
            "hosted_visible",
            true,
            "example.com",
            "alice.example.com",
            Some(NestIdentitySummary {
                handle: "alice.example.com".into(),
                method: "web".into(),
                status: "active".into(),
                did: Some("did:web:alice.example.com".into()),
                ..Default::default()
            }),
        );
        m.refresh().await;
        m.open_delete_confirm();

        let opt = m.snapshot().delete_confirm.expect("card").retire_identity;
        assert!(!opt.available, "did:web has no operation log to tombstone");
        assert_eq!(
            opt.unavailable_reason.map(|r| r.key),
            Some("atproto_settings.delete_retire_unavailable_web".to_string()),
            "a greyed row says why, never a control that errors on press"
        );

        m.set_delete_retire_identity(true);
        let snap = m.snapshot();
        assert!(!snap.delete_confirm.expect("card").retire_identity.selected);
        assert!(snap.error.is_some(), "an impossible tick is refused loudly");
        m.confirm_delete().await;
        assert_eq!(request_tombstone_calls(&api), 0);
    }

    #[tokio::test]
    async fn an_unpublished_plc_identity_cannot_be_retired_yet() {
        let (m, api, _cfg, _o) = machine();
        api.set_status(
            "hosted_visible",
            true,
            "example.com",
            "alice.example.com",
            Some(NestIdentitySummary {
                handle: "alice.example.com".into(),
                method: "plc".into(),
                status: "pending".into(),
                did: None,
                ..Default::default()
            }),
        );
        m.refresh().await;
        m.open_delete_confirm();

        let opt = m.snapshot().delete_confirm.expect("card").retire_identity;
        assert!(!opt.available);
        assert_eq!(
            opt.unavailable_reason.map(|r| r.key),
            Some("atproto_settings.delete_retire_unavailable_unpublished".to_string())
        );
    }

    #[tokio::test]
    async fn the_delete_button_retires_once_the_presence_is_deleted() {
        // § User actions row 4 offers the button for an identity that is
        // "active *or* deactivated". A presence already destroyed has nothing
        // left to destroy, and a control whose only outcome is a no-op is the
        // dead button `open_contest_confirm` refuses to paint.
        let (m, _api, _cfg, _o) = with_hosted_identity().await;
        assert!(m.snapshot().show_delete_presence);

        m.open_delete_confirm();
        m.confirm_delete().await;

        assert!(!m.snapshot().show_delete_presence);
    }

    #[tokio::test]
    async fn a_failed_delete_keeps_the_card_open_and_retry_succeeds() {
        // § Errors & edge cases: the card stays open with the page error
        // populated and nothing changed; retry is safe.
        let (m, api, _cfg, _o) = with_hosted_identity().await;
        m.open_delete_confirm();
        api.fail_delete_presence(AtprotoSettingsApiError::Transient {
            detail: "connection reset".into(),
        });

        m.confirm_delete().await;

        let snap = m.snapshot();
        let card = snap
            .delete_confirm
            .as_ref()
            .expect("the card stays open so the user can retry");
        assert!(!card.in_progress, "the failed attempt released the control");
        assert!(snap.error.is_some(), "the page says why");
        assert_eq!(snap.level, "hosted_visible", "nothing changed");
        assert_eq!(
            snap.identity.as_ref().map(|i| i.status.as_str()),
            Some("active")
        );

        api.clear_delete_presence_failure();
        m.confirm_delete().await;

        let snap = m.snapshot();
        assert!(snap.delete_confirm.is_none());
        assert_eq!(snap.level, "off");
        assert!(snap.error.is_none(), "the retry cleared the error");
    }

    #[tokio::test]
    async fn a_second_device_already_deleted_it_and_that_is_a_success() {
        // `newly_deleted: false` is a retry or a second device — a success,
        // never an error to surface (the reply's own contract).
        let (m, api, _cfg, _o) = with_hosted_identity().await;
        api.set_status(
            "off",
            true,
            "example.com",
            "alice.example.com",
            Some(NestIdentitySummary {
                handle: "alice.example.com".into(),
                method: "plc".into(),
                status: "deleted".into(),
                did: Some("did:plc:abc".into()),
                ..Default::default()
            }),
        );
        m.refresh().await;
        // The page no longer offers the button, so reach the gesture the way a
        // stale render would: directly.
        m.open_delete_confirm();
        assert!(
            m.snapshot().delete_confirm.is_none(),
            "no card over a presence that is already gone"
        );
        assert!(
            m.snapshot().error.is_some(),
            "and it refuses loudly rather than doing nothing (testing.md 11)"
        );
    }

    #[tokio::test]
    async fn there_is_no_card_without_an_identity_to_delete() {
        let (m, api, _cfg, _o) = machine();
        m.refresh().await;
        assert!(!m.snapshot().show_delete_presence);

        m.open_delete_confirm();

        assert!(m.snapshot().delete_confirm.is_none());
        assert!(m.snapshot().error.is_some(), "refuses loudly");
        assert_eq!(delete_calls(&api), 0);
    }

    #[tokio::test]
    async fn a_confirm_with_no_card_open_calls_nothing() {
        // The guard that makes a double-press safe: the first confirm closes
        // the card, so the second has nothing to act on.
        let (m, api, _cfg, _o) = with_hosted_identity().await;
        m.open_delete_confirm();

        m.confirm_delete().await;
        m.confirm_delete().await;

        assert_eq!(delete_calls(&api), 1, "confirming twice destroys once");
    }

    // ── selection stages; confirm mutates ───────────────────────────────

    #[tokio::test]
    async fn selecting_an_effectful_level_stages_a_card_and_mutates_nothing() {
        let (m, api, _cfg, _o) = machine();
        m.refresh().await;

        m.select_level("hosted_visible".into()).await;

        let snap = m.snapshot();
        let card = snap.pending_transition.expect("card staged");
        assert_eq!(card.target_level, "hosted_visible");
        assert!(!card.in_progress);
        assert!(
            card.show_history_backfill,
            "a minting transition offers the second opt-in"
        );
        assert!(!card.lines.is_empty(), "the card states what will happen");
        assert_eq!(snap.level, "off", "the level NEVER changes on selection");
        assert!(
            set_level_calls(&api).is_empty(),
            "no nest mutation before the confirm"
        );
    }

    #[tokio::test]
    async fn confirm_makes_the_one_call_with_mint_parameters_and_converges() {
        let (m, api, cfg, _o) = machine();
        m.refresh().await;
        m.select_level("hosted_visible".into()).await;
        m.set_history_backfill(true);

        m.confirm_transition().await;

        let calls = set_level_calls(&api);
        assert_eq!(calls.len(), 1, "ONE call per confirmed transition");
        let FakeCall::SetIntegrationLevel {
            target_level,
            did_method,
            user_rotation_pub_did_key,
            history_backfill,
        } = &calls[0]
        else {
            unreachable!()
        };
        assert_eq!(target_level, "hosted_visible");
        assert_eq!(did_method, "plc");
        assert!(
            user_rotation_pub_did_key.starts_with("did:key:zDn"),
            "the senior rotation pubkey rides the mint"
        );
        assert!(history_backfill);

        // The key was persisted client-side (sole-writer module) — only the
        // pubkey crossed the seam.
        let stored = cfg.current();
        assert_eq!(stored.rotation_keys.len(), 1);
        assert_eq!(
            &stored.rotation_keys[0].pubkey_did_key,
            user_rotation_pub_did_key
        );

        let snap = m.snapshot();
        assert!(snap.pending_transition.is_none(), "card closed on success");
        assert_eq!(snap.level, "hosted_visible", "converged on nest's reply");
        assert!(snap.error.is_none());
        assert_eq!(
            snap.identity.expect("refresh shows the intent row").status,
            "pending"
        );
    }

    /// THE linkability pin, machine level: after a retirement burned the
    /// old key (its `published_for_dids` names the retired DID), a re-mint
    /// must send a FRESH pubkey into `set_integration_level` — reusing the
    /// old one would put the same `rotationKeys[0]` in the retired DID's
    /// and the new DID's public PLC logs, permanently linking the clean
    /// break the user asked for.
    #[tokio::test]
    async fn a_remint_after_retirement_sends_a_fresh_senior_key() {
        let (m, api, cfg, _o) = machine();
        let store = cfg.clone();
        let old = mint_rotation_key(&store, 1).await.unwrap();
        record_published_binding(&store, "did:plc:retiredoldidentity1111", &old)
            .await
            .unwrap();

        m.refresh().await;
        m.select_level("hosted_visible".into()).await;
        m.confirm_transition().await;

        let calls = set_level_calls(&api);
        assert_eq!(calls.len(), 1);
        let FakeCall::SetIntegrationLevel {
            user_rotation_pub_did_key,
            ..
        } = &calls[0]
        else {
            unreachable!()
        };
        assert_ne!(
            user_rotation_pub_did_key, &old,
            "a re-mint must never reuse the retired identity's senior key"
        );
        assert!(user_rotation_pub_did_key.starts_with("did:key:zDn"));

        // Both keys are held afterwards: the burned one stays (it can still
        // sign the retired DID's log if ever needed), the fresh one rides.
        let ring = cfg.current().rotation_keys;
        assert_eq!(ring.len(), 2);
        assert!(
            ring.iter()
                .any(|k| &k.pubkey_did_key == user_rotation_pub_did_key
                    && k.published_for_dids.is_empty())
        );
        assert!(ring.iter().any(|k| k.pubkey_did_key == old
            && k.published_for_dids == vec!["did:plc:retiredoldidentity1111"]));
    }

    #[tokio::test]
    async fn a_web_method_mint_sends_no_rotation_pubkey() {
        let (m, api, _cfg, _o) = machine();
        m.refresh().await;
        m.set_did_method("web".into());
        m.select_level("hosted_visible".into()).await;

        m.confirm_transition().await;

        let calls = set_level_calls(&api);
        let FakeCall::SetIntegrationLevel {
            did_method,
            user_rotation_pub_did_key,
            ..
        } = &calls[0]
        else {
            unreachable!()
        };
        assert_eq!(did_method, "web");
        assert!(
            user_rotation_pub_did_key.is_empty(),
            "did:web has no rotation keys"
        );
    }

    #[tokio::test]
    async fn off_to_linked_applies_immediately_without_a_card() {
        // The one effect-free rung: apply on select (`ui/atproto.md`
        // § Transition semantics) — still a nest write, just card-less.
        let (m, api, _cfg, _o) = machine();
        m.refresh().await;

        m.select_level("linked".into()).await;

        assert_eq!(set_level_calls(&api).len(), 1);
        let snap = m.snapshot();
        assert_eq!(snap.level, "linked");
        assert!(snap.pending_transition.is_none(), "no card was ever shown");
    }

    #[tokio::test]
    async fn reselecting_the_current_level_closes_the_card_and_calls_nothing() {
        let (m, api, _cfg, _o) = machine();
        m.refresh().await;
        m.select_level("hosted_visible".into()).await;
        assert!(m.snapshot().pending_transition.is_some());

        m.select_level("off".into()).await;

        assert!(m.snapshot().pending_transition.is_none());
        assert!(set_level_calls(&api).is_empty());
    }

    #[tokio::test]
    async fn cancel_closes_the_card_without_any_call() {
        let (m, api, _cfg, _o) = machine();
        m.refresh().await;
        m.select_level("hosted_visible".into()).await;

        m.cancel_transition();

        assert!(m.snapshot().pending_transition.is_none());
        assert!(set_level_calls(&api).is_empty());
        assert_eq!(m.snapshot().level, "off");
    }

    #[tokio::test]
    async fn a_gated_hosted_selection_is_refused_loudly_not_dropped() {
        // testing.md rule 11: the options render disabled, but a command that
        // does arrive must fail on the page error, never vanish.
        let (m, api, _cfg, _o) = machine();
        api.set_status("off", false, "localhost", "", None);
        m.refresh().await;

        m.select_level("hosted_visible".into()).await;

        let snap = m.snapshot();
        assert!(snap.pending_transition.is_none(), "nothing staged");
        assert!(set_level_calls(&api).is_empty(), "nothing sent");
        assert!(snap.error.is_some(), "refused LOUDLY");
    }

    #[tokio::test]
    async fn a_failed_confirm_keeps_the_card_open_with_the_error() {
        // `ui/atproto.md` § Errors & edge cases: transition failure → card
        // stays open, `error-message` populated, level unchanged, retry safe.
        let (m, api, _cfg, _o) = machine();
        m.refresh().await;
        m.select_level("hosted_visible".into()).await;
        api.fail_set_level(AtprotoSettingsApiError::Transient {
            detail: "nest refused".into(),
        });

        m.confirm_transition().await;

        let snap = m.snapshot();
        let card = snap.pending_transition.expect("card still open");
        assert!(!card.in_progress, "confirm re-enabled for retry");
        assert!(snap.error.is_some());
        assert_eq!(snap.level, "off", "level unchanged on failure");
    }

    #[tokio::test]
    async fn selection_is_inert_while_a_confirm_is_in_flight() {
        // Pin the guard synchronously: stage a card, mark it in flight (as
        // the async confirm does), and check the selector refuses to restage.
        let (m, api, _cfg, _o) = machine();
        m.refresh().await;
        m.select_level("hosted_visible".into()).await;
        api.fail_set_level(AtprotoSettingsApiError::Transient {
            detail: "slow".into(),
        });
        {
            let mut s = m.state.lock().unwrap();
            s.pending.as_mut().unwrap().in_progress = true;
        }

        m.select_level("hosted_full".into()).await;
        m.cancel_transition();

        let snap = m.snapshot();
        let card = snap.pending_transition.expect("in-flight card untouched");
        assert_eq!(card.target_level, "hosted_visible");
        assert!(card.in_progress);
    }

    // ── the card copy is plan-driven ────────────────────────────────────

    fn keys(lines: &[LocalizedText]) -> String {
        format!("{lines:?}")
    }

    #[tokio::test]
    async fn a_step_down_card_promises_reversibility_lines() {
        let (m, api, _cfg, _o) = machine();
        api.set_status(
            "hosted_full",
            true,
            "example.com",
            "alice.example.com",
            Some(NestIdentitySummary {
                handle: "alice.example.com".into(),
                method: "plc".into(),
                status: "active".into(),
                did: None,
                ..Default::default()
            }),
        );
        m.refresh().await;

        m.select_level("off".into()).await;

        let card = m.snapshot().pending_transition.expect("card staged");
        let k = keys(&card.lines);
        assert!(k.contains("card_deactivate"), "deactivation stated: {k}");
        assert!(k.contains("card_no_recall"), "no-recall honesty: {k}");
        assert!(k.contains("card_delete_pointer"), "delete pointer: {k}");
        assert!(k.contains("card_suspend_plane"), "plane suspension: {k}");
        assert!(!k.contains("card_mint"), "nothing mints on the way down");
        assert!(
            !card.show_history_backfill,
            "no second opt-in on a step-down"
        );
    }

    #[tokio::test]
    async fn a_reentry_card_promises_restoration_not_a_new_identity() {
        let (m, api, _cfg, _o) = machine();
        api.set_status(
            "off",
            true,
            "example.com",
            "alice.example.com",
            Some(NestIdentitySummary {
                handle: "alice.example.com".into(),
                method: "plc".into(),
                status: "deactivated".into(),
                did: None,
                ..Default::default()
            }),
        );
        m.refresh().await;

        m.select_level("hosted_visible".into()).await;

        let card = m.snapshot().pending_transition.expect("card staged");
        let k = keys(&card.lines);
        assert!(k.contains("card_reactivate"), "restoration stated: {k}");
        assert!(!k.contains("card_mint"), "the same DID — never a re-mint");
        assert!(
            k.contains("card_publish_consent"),
            "projection resumes ⇒ consent restated: {k}"
        );
    }

    #[tokio::test]
    async fn entering_hosted_with_a_link_promises_the_unlink() {
        let (m, api, _cfg, _o) = machine();
        api.set_link(Some(LinkSummary {
            display: "@alice.bsky.social".into(),
        }));
        m.refresh().await;
        assert_eq!(m.snapshot().link.unwrap().display, "@alice.bsky.social");

        m.select_level("hosted_visible".into()).await;

        let card = m.snapshot().pending_transition.expect("card staged");
        let k = keys(&card.lines);
        assert!(k.contains("card_unlink"), "one-backing rule stated: {k}");
        assert!(
            k.contains("@alice.bsky.social"),
            "the card names the account being unlinked: {k}"
        );
    }

    #[tokio::test]
    async fn the_dm_honesty_line_rides_the_full_pds_entry() {
        let (m, api, _cfg, _o) = machine();
        api.set_status(
            "hosted_visible",
            true,
            "example.com",
            "alice.example.com",
            Some(NestIdentitySummary {
                handle: "alice.example.com".into(),
                method: "plc".into(),
                status: "active".into(),
                did: None,
                ..Default::default()
            }),
        );
        m.refresh().await;

        m.select_level("hosted_full".into()).await;

        let card = m.snapshot().pending_transition.expect("card staged");
        let k = keys(&card.lines);
        assert!(k.contains("card_open_plane"), "{k}");
        assert!(k.contains("card_dm_honesty"), "{k}");
        assert!(!k.contains("card_deactivate"), "nothing destructive: {k}");
    }

    // ── gestures on the mint parameters ─────────────────────────────────

    #[tokio::test]
    async fn set_did_method_rejects_junk_loudly() {
        let (m, _api, _cfg, _o) = machine();
        m.set_did_method("sideways".into());
        assert!(m.snapshot().error.is_some());
        assert_eq!(m.snapshot().did_method, "plc", "state unchanged");
    }

    #[tokio::test]
    async fn the_widened_snapshot_still_never_carries_the_rotation_secret() {
        // The custody guard extended to the selector: confirm generates and
        // persists the senior rotation key — the snapshot must carry only
        // renderable state, never the scalar.
        let (m, _api, cfg, _o) = machine();
        m.refresh().await;
        m.select_level("hosted_visible".into()).await;
        m.confirm_transition().await;

        let stored = cfg.current();
        let scalar = stored.rotation_keys[0].secret_scalar.clone();
        let scalar_hex = hex::encode(scalar.as_slice());
        let rendered = format!("{:?}", m.snapshot());
        assert!(
            !rendered.contains(&scalar_hex),
            "rotation-key scalar leaked into the snapshot"
        );
    }
}

#[cfg(test)]
mod custody_tests {
    //! S4-C: the genesis-seniority custody check → critical-alerts wiring.

    use std::sync::Arc;

    use super::*;
    use crate::custody::{FakeGenesisVerifier, MismatchReason, VerifyFailure};
    use crate::nest_api::FakeAtprotoSettingsNestApi;
    use crate::observer::CountingObserver;
    use fauna_client_atproto::identity_store::InMemoryAtprotoIdentityStore;

    const DID: &str = "did:plc:abc123custody";

    struct Setup {
        machine: Arc<AtprotoSettingsMachine>,
        api: Arc<FakeAtprotoSettingsNestApi>,
        registry: Arc<CriticalAlerts>,
        verifier: Arc<FakeGenesisVerifier>,
        senior_key: String,
        custody: InMemoryAtprotoIdentityStore,
    }

    /// A machine whose fake nest reports an active did:plc identity, with a
    /// real senior rotation key seeded in the account's custody.
    async fn setup(verdict: Result<SeniorityVerdict, VerifyFailure>) -> Setup {
        let api = Arc::new(FakeAtprotoSettingsNestApi::new());
        let custody = InMemoryAtprotoIdentityStore::default();
        let senior_key = mint_rotation_key(&custody, 1)
            .await
            .expect("seed senior key");
        let registry = Arc::new(CriticalAlerts::new());
        let verifier = FakeGenesisVerifier::new(verdict);
        let machine = AtprotoSettingsMachine::new(
            CountingObserver::new(),
            api.clone(),
            verifier.clone(),
            Some(registry.clone()),
        );
        machine.set_identity_store(Arc::new(custody.clone()));
        machine.set_credential_store(Arc::new(crate::credentials::FakeCredentialStore::new()));
        api.set_status(
            "hosted_visible",
            true,
            "example.com",
            "alice.example.com",
            Some(NestIdentitySummary {
                handle: "alice.example.com".into(),
                method: "plc".into(),
                status: "active".into(),
                did: Some(DID.into()),
                ..Default::default()
            }),
        );
        Setup {
            machine,
            api,
            registry,
            verifier,
            senior_key,
            custody,
        }
    }

    fn alert_key() -> String {
        format!("atproto-custody:{DID}")
    }

    #[tokio::test]
    async fn mismatch_posts_the_critical_alert_after_confirmation_and_a_pass_clears_it() {
        let s = setup(Ok(SeniorityVerdict::Mismatch(
            MismatchReason::SeniorKeyDiffers {
                standing_index: 0,
                found: Some("did:key:zQ3shBoxKey".into()),
            },
        )))
        .await;

        // First sighting: recorded, NOT alarmed — the one legitimate cause
        // (a sibling device's fresh re-mint key not yet synced into this
        // ring) dissolves with config sync, and a self-clearing critical
        // alert would cost the real alarm its credibility.
        s.machine.refresh().await;
        assert!(
            s.registry.active().is_empty(),
            "a first sighting is a suspect, not an alarm"
        );
        assert_eq!(
            s.verifier.calls(),
            vec![(DID.to_string(), vec![s.senior_key.clone()])],
            "the check compares the published log against the whole held ring"
        );

        // Second consecutive sighting of the SAME contradiction: confirmed.
        s.machine.refresh().await;
        let active = s.registry.active();
        assert_eq!(active.len(), 1, "a confirmed mismatch posts the alert");
        assert_eq!(active[0].key, alert_key());
        assert_eq!(
            active[0].lines[0].key,
            "critical_alerts.atproto_custody_mismatch"
        );
        assert_eq!(
            active[0].lines[0].args.get("handle").map(String::as_str),
            Some("alice.example.com")
        );

        // A mismatch is re-checked every refresh (never cached)…
        s.machine.refresh().await;
        assert_eq!(s.verifier.calls().len(), 3);
        assert_eq!(s.registry.active().len(), 1, "still wrong ⇒ still up");

        // …so a directory that now verifies clears the alarm.
        s.verifier.set_verdict(Ok(SeniorityVerdict::Verified {
            standing_ops: 1,
            observed_seniors: vec![s.senior_key.clone()],
        }));
        s.machine.refresh().await;
        assert!(s.registry.active().is_empty(), "a passing re-check clears");
    }

    /// The debounce's whole point: a contradiction that resolves before the
    /// second look (the sibling-device fresh-key sync race) never alarms.
    #[tokio::test]
    async fn a_transient_mismatch_resolved_before_confirmation_never_alarms() {
        let s = setup(Ok(SeniorityVerdict::Mismatch(
            MismatchReason::SeniorKeyDiffers {
                standing_index: 0,
                found: Some("did:key:zDnaeSiblingFresh".into()),
            },
        )))
        .await;
        s.machine.refresh().await;
        assert!(s.registry.active().is_empty());

        s.verifier.set_verdict(Ok(SeniorityVerdict::Verified {
            standing_ops: 1,
            observed_seniors: vec![s.senior_key.clone()],
        }));
        s.machine.refresh().await;
        assert!(
            s.registry.active().is_empty(),
            "resolved before confirmation — the user never saw a flash"
        );
    }

    /// **The genesis-time TOFU case.**
    ///
    /// This test used to assert the opposite, and its own doc named the reason
    /// it was wrong: *"the verdict never passes, so nothing is ever burned into
    /// `published_for_dids`"*. That is not a description of a benign departure —
    /// it is a description of **the genesis-time compromise itself**, the exact
    /// case feeder #1 exists to detect. A box could therefore mint under its own
    /// senior key, let the alarm post, then answer `identity: null` and watch
    /// the accusation clear itself.
    ///
    /// The old rule rested on "nothing would ever re-verify it, so the alert
    /// would be permanent". Both halves are now false: the client **freezes**
    /// every DID the nest names ([`record_nest_named_did`]), so the audit floor
    /// re-derives this verdict from the public log on every sweep. The alarm is
    /// not stranded — it is *maintained*, by the directory rather than by the
    /// box.
    ///
    /// Re-stated rather than deleted, and the honest arm it also covered is
    /// split out into [`a_departed_did_with_no_client_side_record_at_all_clears`].
    ///
    /// Mutation check: drop the `nest_named_dids` half of the clear condition in
    /// [`AtprotoSettingsMachine::clear_departed_alarm_if_earned`] and this goes
    /// red.
    #[tokio::test]
    async fn a_departed_did_the_client_froze_keeps_its_alarm() {
        let s = setup(Ok(SeniorityVerdict::Mismatch(
            MismatchReason::SeniorKeyDiffers {
                standing_index: 0,
                found: Some("did:key:zQ3shBoxKey".into()),
            },
        )))
        .await;
        s.machine.refresh().await;
        s.machine.refresh().await;
        assert_eq!(s.registry.active().len(), 1, "confirmed and posted");
        assert_eq!(
            s.custody.current().nest_named_dids,
            vec![DID.to_string()],
            "precondition: naming the identity at all froze the claim — nothing here \
             required a passing verdict"
        );

        s.api
            .set_status("off", true, "example.com", "alice.example.com", None);
        s.machine.refresh().await;
        assert_eq!(
            s.registry.active().len(),
            1,
            "the box does not get to unsay what it already said: {:?}",
            s.registry.active()
        );
        assert_eq!(s.registry.active()[0].key, alert_key());
    }

    /// The honest half of the arm re-taken above: a client with **no** record of
    /// this DID at all — nothing burned and nothing frozen — has only ever had
    /// the nest's word for it, so the alarm goes when the word does.
    ///
    /// Reachable in production when the freeze never landed: a config write
    /// that failed (the field degrades to *absent* by design). Modelled by emptying the
    /// frozen set after the alarm posts.
    #[tokio::test]
    async fn a_departed_did_with_no_client_side_record_at_all_clears() {
        let s = setup(Ok(SeniorityVerdict::Mismatch(
            MismatchReason::SeniorKeyDiffers {
                standing_index: 0,
                found: Some("did:key:zQ3shBoxKey".into()),
            },
        )))
        .await;
        s.machine.refresh().await;
        s.machine.refresh().await;
        assert_eq!(s.registry.active().len(), 1, "confirmed and posted");

        let mut cfg = s.custody.current();
        cfg.nest_named_dids.clear();
        assert!(
            cfg.rotation_keys
                .iter()
                .all(|k| k.published_for_dids.is_empty()),
            "precondition: nothing was ever burned either — the verdict never passed"
        );
        s.custody.replace(cfg);

        s.api
            .set_status("off", true, "example.com", "alice.example.com", None);
        s.machine.refresh().await;
        assert!(
            s.registry.active().is_empty(),
            "with no client-side tie of any kind, the alarm rested on nest testimony \
             alone and goes with it: {:?}",
            s.registry.active()
        );
    }

    /// Drive the machine to a **confirmed, standing** custody alarm for a DID
    /// this client has independently observed itself published senior for —
    /// the state leg B used to be able to take down with one null field.
    ///
    /// The burn is seeded directly rather than driven through a passing
    /// convergence, because a pass populates the machine's `custody_ok` cache
    /// and a later contradiction would never be looked at again in one session.
    /// The real sequence spans sessions (verify, later the box rotates); what
    /// matters to the fix is the *state*, which is what this reproduces.
    async fn setup_with_a_standing_alarm_for_a_protected_did() -> Setup {
        let s = setup(Ok(SeniorityVerdict::Mismatch(
            MismatchReason::SeniorKeyDiffers {
                standing_index: 0,
                found: Some("did:key:zQ3shBoxKey".into()),
            },
        )))
        .await;
        s.machine.refresh().await;
        s.machine.refresh().await;
        assert_eq!(
            s.registry.active().len(),
            1,
            "precondition: confirmed alarm"
        );

        let store = s.custody.clone();
        record_published_binding(&store, DID, &s.senior_key)
            .await
            .expect("seed the directory-derived binding");
        assert_eq!(
            s.custody.current().rotation_keys[0].published_for_dids,
            vec![DID.to_string()],
            "precondition: the client independently observed this DID published"
        );
        s
    }

    /// **The finding's leg B.** A nest that stops naming a DID does not get to
    /// take down the alarm accusing it. The client independently observed this
    /// DID published for one of its own keys, and the published log still shows
    /// a senior key that is not ours — so the banner STANDS, whatever the nest
    /// says about whose identity it is.
    ///
    /// Mutation check: restore the unconditional
    /// `alerts.clear(&alert_key(&old))` and this goes red.
    #[tokio::test]
    async fn a_nest_that_stops_naming_a_protected_did_cannot_clear_its_alarm() {
        let s = setup_with_a_standing_alarm_for_a_protected_did().await;

        s.api
            .set_status("off", true, "example.com", "alice.example.com", None);
        s.machine.refresh().await;

        assert_eq!(
            s.registry.active().len(),
            1,
            "a null identity field is not evidence of anything: {:?}",
            s.registry.active()
        );
        assert_eq!(s.registry.active()[0].key, alert_key());
    }

    /// The legitimate retirement the clear exists for — and the reason it needs
    /// no separately stored "the client retired this" flag. An OWN-signed
    /// tombstone in the public log is the condition re-checked and found
    /// resolved, and it is there whether this device or a sibling signed it
    /// (the attribution is against the synced ring, not this device's key).
    #[tokio::test]
    async fn a_departed_did_terminally_retired_in_the_public_log_clears() {
        let s = setup_with_a_standing_alarm_for_a_protected_did().await;

        s.verifier.set_verdict(Ok(SeniorityVerdict::Mismatch(
            MismatchReason::OwnRetirement { standing_index: 1 },
        )));
        s.api
            .set_status("off", true, "example.com", "alice.example.com", None);
        s.machine.refresh().await;

        assert!(
            s.registry.active().is_empty(),
            "a retired identity's alarm is not stranded: {:?}",
            s.registry.active()
        );
    }

    /// A
    /// tombstone the held ring did NOT sign is someone else's destruction of
    /// an identity this client protects — the bridge's listed junior key is
    /// the named suspect — and it must not do what an own retirement does.
    /// Before this split, the box that tombstoned the DID and then stopped
    /// naming it took the standing alarm down with the same move.
    #[tokio::test]
    async fn a_foreign_signed_tombstone_leaves_a_departed_dids_alarm_standing() {
        let s = setup_with_a_standing_alarm_for_a_protected_did().await;

        s.verifier.set_verdict(Ok(SeniorityVerdict::Mismatch(
            MismatchReason::RetirementByUnheldKey { standing_index: 1 },
        )));
        s.api
            .set_status("off", true, "example.com", "alice.example.com", None);
        s.machine.refresh().await;

        assert_eq!(
            s.registry.active().len(),
            1,
            "an unattributable terminal act never clears the alarm: {:?}",
            s.registry.active()
        );
    }

    /// Unreachable is not resolved. A directory that merely went offline must
    /// not be able to do what the nest just failed to.
    #[tokio::test]
    async fn an_unreadable_directory_leaves_a_departed_dids_alarm_standing() {
        let s = setup_with_a_standing_alarm_for_a_protected_did().await;

        s.verifier
            .set_verdict(Err(VerifyFailure::Fetch("directory offline".into())));
        s.api
            .set_status("off", true, "example.com", "alice.example.com", None);
        s.machine.refresh().await;

        assert_eq!(s.registry.active().len(), 1, "{:?}", s.registry.active());
    }

    /// A Verified read burns the observed senior key against the DID — the
    /// published-for fact that keeps `mint_rotation_key` from ever reusing
    /// it (and the custody-check leg of the two binding writers).
    #[tokio::test]
    async fn a_verified_pass_burns_the_observed_senior_for_the_did() {
        let s = setup(Err(VerifyFailure::Fetch("not yet".into()))).await;
        s.verifier.set_verdict(Ok(SeniorityVerdict::Verified {
            standing_ops: 1,
            observed_seniors: vec![s.senior_key.clone()],
        }));
        s.machine.refresh().await;

        let ring = s.custody.current().rotation_keys;
        assert_eq!(ring.len(), 1);
        assert_eq!(
            ring[0].published_for_dids,
            vec![DID.to_string()],
            "the log-derived binding is recorded"
        );
    }

    /// The pass cache is keyed on the whole ring: a key syncing in (a
    /// sibling's fresh re-mint) re-verifies rather than inheriting a stale
    /// pass for a ring that no longer exists.
    #[tokio::test]
    async fn a_ring_change_reverifies_rather_than_trusting_a_stale_pass() {
        let s = setup(Ok(SeniorityVerdict::Verified {
            standing_ops: 1,
            observed_seniors: vec![],
        }))
        .await;
        s.machine.refresh().await;
        s.machine.refresh().await;
        assert_eq!(s.verifier.calls().len(), 1, "cached while the ring stands");

        // A second key enters the ring (burn the first, mint a fresh one —
        // the re-mint shape).
        let store = s.custody.clone();
        record_published_binding(&store, "did:plc:elsewhere", &s.senior_key)
            .await
            .unwrap();
        let fresh = mint_rotation_key(&store, 2).await.unwrap();
        assert_ne!(fresh, s.senior_key);

        s.machine.refresh().await;
        let calls = s.verifier.calls();
        assert_eq!(calls.len(), 2, "a changed ring re-verifies");
        assert_eq!(
            calls[1].1,
            vec![s.senior_key.clone(), fresh],
            "…against the grown ring"
        );
    }

    #[tokio::test]
    async fn a_passing_verdict_is_cached_for_the_session() {
        let s = setup(Ok(SeniorityVerdict::Verified {
            standing_ops: 1,
            observed_seniors: vec![],
        }))
        .await;
        s.machine.refresh().await;
        s.machine.refresh().await;
        assert_eq!(
            s.verifier.calls().len(),
            1,
            "a verified (did, key) pair is not re-fetched"
        );
        assert!(s.registry.active().is_empty());
    }

    #[tokio::test]
    async fn directory_failure_is_quiet_and_retried() {
        let s = setup(Err(VerifyFailure::Fetch("offline".into()))).await;
        s.machine.refresh().await;
        assert!(
            s.registry.active().is_empty(),
            "unreachable ≠ compromised — no alarm"
        );
        s.machine.refresh().await;
        assert_eq!(
            s.verifier.calls().len(),
            2,
            "failure is retried, not cached"
        );
    }

    #[tokio::test]
    async fn did_web_and_pending_identities_are_skipped() {
        let s = setup(Ok(SeniorityVerdict::Verified {
            standing_ops: 1,
            observed_seniors: vec![],
        }))
        .await;
        s.api.set_status(
            "hosted_visible",
            true,
            "example.com",
            "alice.example.com",
            Some(NestIdentitySummary {
                handle: "alice.example.com".into(),
                method: "web".into(),
                status: "active".into(),
                did: Some("did:web:alice.example.com".into()),
                ..Default::default()
            }),
        );
        s.machine.refresh().await;

        s.api.set_status(
            "hosted_visible",
            true,
            "example.com",
            "alice.example.com",
            Some(NestIdentitySummary {
                handle: "alice.example.com".into(),
                method: "plc".into(),
                status: "pending".into(),
                did: None,
                ..Default::default()
            }),
        );
        s.machine.refresh().await;

        assert!(
            s.verifier.calls().is_empty(),
            "no did:plc DID ⇒ nothing to audit"
        );
    }

    #[tokio::test]
    async fn a_device_without_the_rotation_key_stays_quiet() {
        // E.g. a second device before the custody rows sync: cannot verify ⇒
        // no fetch, no alarm — never a false positive from missing local state.
        let api = Arc::new(FakeAtprotoSettingsNestApi::new());
        let registry = Arc::new(CriticalAlerts::new());
        let verifier = FakeGenesisVerifier::new(Ok(SeniorityVerdict::Verified {
            standing_ops: 1,
            observed_seniors: vec![],
        }));
        let machine = AtprotoSettingsMachine::new(
            CountingObserver::new(),
            api.clone(),
            verifier.clone(),
            Some(registry.clone()),
        );
        // An empty custody: no key seeded.
        machine.set_identity_store(Arc::new(InMemoryAtprotoIdentityStore::default()));
        machine.set_credential_store(Arc::new(crate::credentials::FakeCredentialStore::new()));
        api.set_status(
            "hosted_visible",
            true,
            "example.com",
            "alice.example.com",
            Some(NestIdentitySummary {
                handle: "alice.example.com".into(),
                method: "plc".into(),
                status: "active".into(),
                did: Some(DID.into()),
                ..Default::default()
            }),
        );
        machine.refresh().await;
        assert!(verifier.calls().is_empty());
        assert!(registry.active().is_empty());
    }
}

#[cfg(test)]
mod retirement_tests {
    //! S5 slice 5b: the converge pass that publishes the PLC tombstone the user
    //! opted into, once the delete-presence sweep has finished.

    use std::sync::Arc;

    use super::*;
    use crate::nest_api::{AtprotoSettingsApiError, FakeAtprotoSettingsNestApi, FakeCall};
    use crate::observer::CountingObserver;
    use crate::retirement::{FakeActorCall, FakeLogState, FakeTombstoneActor};
    use fauna_client_atproto::identity_store::InMemoryAtprotoIdentityStore;

    const DID: &str = "did:plc:abc123retiring";

    struct Setup {
        machine: Arc<AtprotoSettingsMachine>,
        api: Arc<FakeAtprotoSettingsNestApi>,
        actor: Arc<FakeTombstoneActor>,
        senior_key: String,
        custody: InMemoryAtprotoIdentityStore,
    }

    /// [`setup`], but the user never ticked the opt-in on any device — the
    /// nest's `tombstone_requested` is testimony with no client-side consent
    /// record behind it, which is the exact attack shape.
    async fn setup_unconsented(identity_status: &str, tombstone_requested: bool) -> Setup {
        setup_inner(identity_status, tombstone_requested, false).await
    }

    /// A machine whose fake nest reports `identity_status`, with a real senior
    /// rotation key seeded in the account's custody and the user's own tombstone
    /// consent recorded beside it (the ordinary retirement fixture: the tick
    /// happened, possibly on another device — consent syncs with the ring).
    async fn setup(identity_status: &str, tombstone_requested: bool) -> Setup {
        setup_inner(identity_status, tombstone_requested, true).await
    }

    async fn setup_inner(
        identity_status: &str,
        tombstone_requested: bool,
        consented: bool,
    ) -> Setup {
        let api = Arc::new(FakeAtprotoSettingsNestApi::new());
        let custody = InMemoryAtprotoIdentityStore::default();
        let senior_key = mint_rotation_key(&custody, 1)
            .await
            .expect("seed senior key");
        if consented {
            record_tombstone_consent(&custody, DID)
                .await
                .expect("seed the user's own opt-in consent");
        }
        let tombstone_actor = FakeTombstoneActor::new();
        let machine = AtprotoSettingsMachine::new_with_seams(
            CountingObserver::new(),
            api.clone(),
            crate::custody::FakeGenesisVerifier::new(Err(crate::custody::VerifyFailure::Fetch(
                "test default: directory unreachable".into(),
            ))),
            None,
            None,
            tombstone_actor.clone(),
            crate::contest::FakeContestActor::new(),
        );
        machine.set_identity_store(Arc::new(custody.clone()));
        machine.set_credential_store(Arc::new(crate::credentials::FakeCredentialStore::new()));
        api.set_status(
            "off",
            true,
            "example.com",
            "alice.example.com",
            Some(NestIdentitySummary {
                handle: "alice.example.com".into(),
                method: "plc".into(),
                status: identity_status.into(),
                tombstone_requested,
                did: Some(DID.into()),
            }),
        );
        Setup {
            machine,
            api,
            actor: tombstone_actor,
            senior_key,
            custody,
        }
    }

    fn recorded(api: &FakeAtprotoSettingsNestApi) -> Vec<String> {
        api.calls()
            .into_iter()
            .filter_map(|c| match c {
                FakeCall::RecordTombstone { prev_cid } => Some(prev_cid),
                _ => None,
            })
            .collect()
    }

    /// The whole pass: one converge step (sweep observation, signing and
    /// submission all decided off one read of the log — the ordering is
    /// pinned inside `tombstone::retirement_step` + `converge_retirement`),
    /// then nest is told. Nothing here is a user gesture — the tick that
    /// recorded the intent happened in an earlier session, possibly on
    /// another device, which is the reason this is a converge pass at all.
    #[tokio::test]
    async fn a_finished_sweep_publishes_the_tombstone_and_reports_it() {
        let s = setup("deleted", true).await;
        // Snapshot BEFORE the pass: the post-retire burn writes the binding
        // into the stored ring, but the converge call must have carried the
        // ring as it stood when the pass began.
        let ring_at_pass_start = stored_ring(&s).await;
        s.machine.refresh().await;

        assert_eq!(
            s.actor.calls(),
            vec![FakeActorCall {
                did: DID.into(),
                // The ring the machine passed is the STORED one, scalars
                // intact — the converge step selects the key this DID's
                // published head lists, so it needs every held key.
                held_keys: ring_at_pass_start,
            }],
            "one converge call, carrying the whole stored ring"
        );
        assert_eq!(
            recorded(&s.api),
            vec!["bafyhead".to_string()],
            "and nest learns the outcome, since it has no other way to"
        );
        assert_eq!(
            s.machine.snapshot().identity.expect("identity").status,
            "tombstoned",
            "the page repaints on the new terminal status without a second fetch"
        );
    }

    async fn stored_ring(s: &Setup) -> Vec<fauna_core::data::AtprotoRotationKey> {
        let _ = &s.senior_key;
        s.custody.current().rotation_keys
    }

    /// An unfinished sweep submits NOTHING. This is the ordering the whole act
    /// depends on: a tombstoned DID stops resolving, and a relay that cannot
    /// resolve a DID cannot verify that DID's commits — so retiring before the
    /// deletes are published leaves our repo gone and every network copy
    /// standing, the precise failure the delete flow exists to prevent.
    #[tokio::test]
    async fn a_sweep_still_running_submits_nothing() {
        let s = setup("deleted", true).await;
        s.actor.set_log(FakeLogState::Live {
            sweep_finished: false,
        });
        s.machine.refresh().await;

        assert!(
            recorded(&s.api).is_empty(),
            "nothing submitted, nothing reported"
        );
        assert_eq!(
            s.machine.snapshot().identity.expect("identity").status,
            "deleted",
            "and the identity stays exactly where nest has it"
        );
    }

    /// **The pin: nest testimony alone must never make this
    /// client sign a tombstone.** Both trigger fields (`tombstone_requested`,
    /// `status = "deleted"`) are nest-authored wire values a hostile box can
    /// write into its own DB — the RPC guard guards the RPC, not the row — so
    /// the converge pass acts only when a consent record the nest cannot
    /// author agrees. Here the intent is set, the sweep is finished, the ring
    /// is held — everything the box can arrange is arranged — and nothing may
    /// be signed, submitted, or reported.
    #[tokio::test]
    async fn an_intent_without_client_consent_signs_nothing() {
        let s = setup_unconsented("deleted", true).await;
        s.machine.refresh().await;

        assert!(
            s.actor.calls().is_empty(),
            "no directory read, no signature: {:?}",
            s.actor.calls()
        );
        assert!(recorded(&s.api).is_empty(), "and nothing reported");
        assert_eq!(
            s.machine.snapshot().identity.expect("identity").status,
            "deleted",
            "the reversible state is untouched"
        );
    }

    /// The opt-in gesture's ordering is a security invariant: consent lands
    /// client-side BEFORE the nest intent, so a crash (here: a failed RPC)
    /// between the two leaves consent-without-intent — inert, re-tickable —
    /// never intent-without-consent, which the converge pass would refuse
    /// forever.
    #[tokio::test]
    async fn the_gesture_records_consent_before_the_nest_intent() {
        let s = setup_unconsented("deleted", false).await;
        s.machine.refresh().await; // the ceremony runs on a painted page
        s.api
            .fail_request_tombstone(AtprotoSettingsApiError::Transient {
                detail: "transport.disconnected".into(),
            });
        let outcome = s.machine.request_tombstone().await;

        let consents = s.custody.current().tombstone_consents;
        assert_eq!(
            consents,
            vec![DID.to_string()],
            "consent survived the failed RPC — written first"
        );
        assert!(
            outcome.is_err(),
            "and the failure is reported to the ceremony, not swallowed"
        );
    }

    /// The whole fixed path, user gesture to terminal status: the tick records
    /// consent + intent, the same gesture's refresh runs the converge pass,
    /// and with both records agreeing the tombstone publishes and reports.
    #[tokio::test]
    async fn the_gesture_then_converges_end_to_end() {
        let s = setup_unconsented("deleted", false).await;
        s.machine.refresh().await; // the ceremony runs on a painted page
        s.machine
            .request_tombstone()
            .await
            .expect("consent + intent recorded");
        // The ceremony's own refresh is what runs the converge pass.
        s.machine.refresh().await;

        assert_eq!(
            recorded(&s.api),
            vec!["bafyhead".to_string()],
            "published and reported off the gesture's own refresh"
        );
        assert_eq!(
            s.machine.snapshot().identity.expect("identity").status,
            "tombstoned"
        );
    }

    /// An unanswerable directory is never a licence to retire early. The
    /// intent is durable nest-side, so the next convergence simply asks again.
    #[tokio::test]
    async fn an_unreachable_pds_retries_rather_than_retiring_blind() {
        let s = setup("deleted", true).await;
        s.actor.set_log(FakeLogState::Unreachable);
        s.machine.refresh().await;

        assert!(recorded(&s.api).is_empty());

        // The retry is the whole point of a converge pass.
        s.actor.set_log(FakeLogState::Live {
            sweep_finished: true,
        });
        s.machine.refresh().await;
        assert_eq!(recorded(&s.api), vec!["bafyhead".to_string()]);
    }

    /// The retire-path leg of the two binding writers: a published
    /// tombstone burns the key that signed it against the retired DID, so a
    /// later mint can never reuse it. This leg is the guaranteed one — the
    /// custody check's Verified arm may never have run, but a retirement
    /// cannot complete without passing here.
    #[tokio::test]
    async fn a_published_retirement_burns_the_signing_key() {
        let s = setup("deleted", true).await;
        s.machine.refresh().await;

        let ring = s.custody.current().rotation_keys;
        assert_eq!(ring.len(), 1);
        assert_eq!(ring[0].pubkey_did_key, s.senior_key);
        assert_eq!(
            ring[0].published_for_dids,
            vec![DID.to_string()],
            "the signing key is burned against the retired DID"
        );
    }

    /// A log already carrying a tombstone is still REPORTED, with an empty
    /// `prev_cid` because this client chained nothing itself. That report is
    /// what closes the crash window between submitting and reporting — and what
    /// lets a second device finish what a first one started.
    #[tokio::test]
    async fn an_already_retired_log_is_reported_rather_than_treated_as_done() {
        let s = setup("deleted", true).await;
        s.actor.set_log(FakeLogState::Tombstoned);
        s.machine.refresh().await;

        assert_eq!(
            recorded(&s.api),
            vec![String::new()],
            "reported, with no prev — the wire's own reading of this case"
        );
        assert_eq!(
            s.machine.snapshot().identity.expect("identity").status,
            "tombstoned"
        );
    }

    /// Nothing is attempted without the durable opt-in, and nothing is
    /// attempted before the presence is recorded deleted. Both are nest's to
    /// decide — the client reads them back rather than deciding for itself,
    /// which is what keeps `ui/atproto.md` § Don't do these ("never a
    /// step-down, never implicit") true from this side too.
    #[tokio::test]
    async fn nothing_is_attempted_outside_the_delete_ceremony() {
        for (status, requested) in [
            ("active", true),
            ("deactivated", true),
            ("deleted", false),
            ("tombstoned", true),
        ] {
            let s = setup(status, requested).await;
            s.machine.refresh().await;
            assert!(
                s.actor.calls().is_empty(),
                "status={status} tombstone_requested={requested} must not even ASK"
            );
            assert!(recorded(&s.api).is_empty());
        }
    }

    /// THE recovery property, run END TO END across two convergences on a
    /// COUPLED directory state (the 2026-07-29 review's red-first pin): a
    /// published tombstone that nest failed to record MUST be re-reported by
    /// a later pass. The fake's own submit flips its modelled log to
    /// Tombstoned — the same coupling the real directory has — so pass 2's
    /// converge answers AlreadyRetired off the state pass 1 created; a fake
    /// holding "sweep finished" and "already retired" as two independent
    /// knobs cannot go red for the deadlock this pins against (the old
    /// two-call seam quiet-retried forever on the tombstone's own service-less
    /// head, so the row wedged at 'deleted' and re-entry re-served a dead
    /// DID).
    #[tokio::test]
    async fn a_failed_report_is_re_reported_by_the_next_pass() {
        let s = setup("deleted", true).await;
        s.api
            .fail_record_tombstone(crate::nest_api::AtprotoSettingsApiError::Transient {
                detail: "nest unreachable".into(),
            });

        // Pass 1: the tombstone PUBLISHES, the report fails.
        s.machine.refresh().await;
        assert_eq!(recorded(&s.api), vec!["bafyhead".to_string()]);
        assert_eq!(
            s.machine.snapshot().identity.expect("identity").status,
            "deleted",
            "the machine does not claim a terminal state nest never acknowledged"
        );

        // Pass 2: the transient has passed. The directory's log — the SAME
        // log pass 1 wrote — now answers AlreadyRetired, and the report must
        // land and converge the row to its terminal status.
        s.api.clear_record_tombstone_failure();
        s.machine.refresh().await;
        assert_eq!(
            recorded(&s.api),
            vec!["bafyhead".to_string(), String::new()],
            "re-reported with the empty prev the wire defines for already-retired"
        );
        assert_eq!(
            s.machine.snapshot().identity.expect("identity").status,
            "tombstoned",
            "the wedge is unrepresentable: the second pass completes the act"
        );
    }

    /// The plan-side half of the same lockout door: an actor whose identity
    /// is genuinely retired stages a MINT card, never a reactivation — a
    /// 'tombstoned' row must not offer to re-serve a DID that resolves
    /// nowhere.
    #[tokio::test]
    async fn a_tombstoned_identity_stages_a_mint_card() {
        let s = setup("tombstoned", false).await;
        s.machine.refresh().await;
        s.machine.select_level("hosted_visible".into()).await;
        let card = s
            .machine
            .snapshot()
            .pending_transition
            .expect("card staged");
        assert!(
            card.show_history_backfill,
            "the staged plan MINTS a fresh identity (backfill opt-in only rides a mint)"
        );
    }

    /// A did:web identity has no operation log to tombstone — its custody IS
    /// domain custody — so the pass must not even reach for the directory.
    #[tokio::test]
    async fn a_did_web_identity_is_never_asked_about() {
        let s = setup("deleted", true).await;
        s.api.set_status(
            "off",
            true,
            "example.com",
            "alice.example.com",
            Some(NestIdentitySummary {
                handle: "alice.example.com".into(),
                method: "web".into(),
                status: "deleted".into(),
                tombstone_requested: true,
                did: Some("did:web:alice.example.com".into()),
            }),
        );
        s.machine.refresh().await;
        assert!(s.actor.calls().is_empty());
    }
}

#[cfg(test)]
mod consent_tests {
    //! F4 rung 2: the OAuth consent ceremony's app half — the pending-request
    //! rows the approval card renders, and the answer that resolves one.

    use std::sync::Arc;

    use super::*;
    use crate::nest_api::{
        AtprotoSettingsApiError, FakeAtprotoSettingsNestApi, FakeCall, NestConsentRow,
    };
    use crate::observer::CountingObserver;

    fn machine() -> (Arc<AtprotoSettingsMachine>, Arc<FakeAtprotoSettingsNestApi>) {
        let api = Arc::new(FakeAtprotoSettingsNestApi::new());
        let verifier = crate::custody::FakeGenesisVerifier::new(Err(
            crate::custody::VerifyFailure::Fetch("test default: directory unreachable".into()),
        ));
        let m = AtprotoSettingsMachine::new(CountingObserver::new(), api.clone(), verifier, None);
        m.set_credential_store(Arc::new(crate::credentials::FakeCredentialStore::new()));
        (m, api)
    }

    fn consent(id: &[u8], code: &str, scopes: &[&str]) -> NestConsentRow {
        NestConsentRow {
            consent_id: id.to_vec(),
            code: code.into(),
            client_id: "https://app.example.com/client-metadata.json".into(),
            client_name: Some("Example App".into()),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            ..NestConsentRow::default()
        }
    }

    /// The row the card renders is a transcription of the nest's row, with ONE
    /// derived field: the scope wording.
    ///
    /// The wording assertion is two-sided on purpose. Asserting only that the
    /// text mentions the collection would pass on the raw scope string too
    /// (`repo:app.bsky.feed.post` contains it), so the pin *also* asserts the
    /// description is not the scope verbatim — which is exactly what deleting
    /// the `describe_scope` map produces.
    #[tokio::test]
    async fn a_pending_request_renders_as_a_card_row_with_described_scopes() {
        let (m, api) = machine();
        api.set_pending_consents(vec![consent(
            &[0xAB, 0xCD],
            "ABC-DEF",
            &["atproto", "repo:app.bsky.feed.post"],
        )]);

        m.refresh().await;

        let snap = m.snapshot();
        assert_eq!(snap.consents.len(), 1);
        let row = &snap.consents[0];
        assert_eq!(row.consent_id_hex, "abcd");
        assert_eq!(row.code, "ABC-DEF");
        assert_eq!(
            row.client_id,
            "https://app.example.com/client-metadata.json"
        );
        assert_eq!(row.client_name.as_deref(), Some("Example App"));
        assert_eq!(row.scope_descriptions.len(), 2);
        for (described, raw) in row
            .scope_descriptions
            .iter()
            .zip(["atproto", "repo:app.bsky.feed.post"])
        {
            assert_ne!(
                described, raw,
                "the card must render describe_scope's wording, not the raw scope"
            );
        }
        assert!(
            row.scope_descriptions[1].contains("app.bsky.feed.post"),
            "a collection-narrowed repo scope names its collection: {:?}",
            row.scope_descriptions[1]
        );
    }

    /// A permission set reaches the card as its **identity plus every member**,
    /// not as its author's one-line summary (`atproto-pds-full.md:334`).
    ///
    /// The member assertion is two-sided for the same reason the flat list's
    /// is: a description that merely mentioned the collection would pass on the
    /// raw scope string too. And the members are asserted *present as their own
    /// lines* — a set-level `details` standing in for them is exactly the
    /// shape the ruling forbids, because a user approving "Calendar sync"
    /// would then have no surface that says what it means.
    #[tokio::test]
    async fn a_permission_set_renders_its_identity_and_every_member() {
        let (m, api) = machine();
        let mut row = consent(&[0x01], "SET-ABC", &["atproto", "repo:com.example.event"]);
        row.sets = vec![NestConsentSet {
            nsid: "com.example.calendar.appPerms".into(),
            title: Some("Calendar sync".into()),
            details: Some("Keeps your calendar in step.".into()),
            members: vec!["repo:com.example.event".into()],
        }];
        api.set_pending_consents(vec![row]);

        m.refresh().await;

        let card = &m.snapshot().consents[0];
        assert_eq!(card.sets.len(), 1);
        let set = &card.sets[0];
        assert_eq!(
            set.nsid, "com.example.calendar.appPerms",
            "the NSID is the identity and crosses verbatim"
        );
        assert_eq!(set.title.as_deref(), Some("Calendar sync"));
        assert_eq!(set.details.as_deref(), Some("Keeps your calendar in step."));
        assert_eq!(set.member_descriptions.len(), 1);
        assert_ne!(
            set.member_descriptions[0], "repo:com.example.event",
            "members are worded by describe_scope, not echoed raw"
        );
        assert!(set.member_descriptions[0].contains("com.example.event"));
        assert_eq!(
            set.member_descriptions[0], card.scope_descriptions[1],
            "a member and the same scope in the flat list are ONE wording — a \
             set is not a second vocabulary"
        );
    }

    /// The set's author is a **second** attacker, and the fence that catches
    /// `client_name` catches them in the same pass.
    ///
    /// `title`/`details` come from a Lexicon record published by whatever DID
    /// the NSID's authority names — a different party from the client, and no
    /// more reviewed. tui interpolates card strings into a multi-line body, so
    /// an unstripped newline here owns ROWS of the card, including a forged
    /// heading above a member list the attacker wrote. Stripping in the shared
    /// machine is what makes it hold for all 7 apps at once; six per-app fixes
    /// is the shape this pin exists to prevent.
    #[tokio::test]
    async fn a_set_title_cannot_forge_card_rows() {
        let (m, api) = machine();
        let mut row = consent(&[0x02], "SET-DEF", &["atproto"]);
        row.sets = vec![NestConsentSet {
            nsid: "com.example.appPerms".into(),
            title: Some("Harmless\n  • Full access to everything".into()),
            details: Some("line\r\nbreak".into()),
            members: vec!["atproto".into()],
        }];
        api.set_pending_consents(vec![row]);

        m.refresh().await;

        let set = &m.snapshot().consents[0].sets[0];
        let title = set.title.as_deref().unwrap();
        assert!(
            !title.contains('\n') && !title.contains('\r'),
            "a set title must not carry row structure: {title:?}"
        );
        let details = set.details.as_deref().unwrap();
        assert!(!details.contains('\n') && !details.contains('\r'));
    }

    /// The overwhelmingly common request names no set, and it must render
    /// exactly as it did before PS-b — an empty grouping, never an invented
    /// "no permission sets" row for six apps to paint.
    #[tokio::test]
    async fn a_request_naming_no_set_carries_no_set_rows() {
        let (m, api) = machine();
        api.set_pending_consents(vec![consent(&[0x03], "NOP-QRS", &["atproto"])]);

        m.refresh().await;

        assert!(m.snapshot().consents[0].sets.is_empty());
    }

    /// The wording is the SAME function the browser's `/oauth/authorize` page
    /// renders — the two surfaces sit side by side during the ceremony, and a
    /// divergence between them is the doubt the binding code exists to remove.
    /// Pinned by equality against that one owner, so a hand-written second
    /// wording in this crate reddens here rather than being noticed by a user
    /// mid-consent.
    #[tokio::test]
    async fn the_card_wording_is_the_one_the_browser_page_renders() {
        let (m, api) = machine();
        let scopes = ["atproto", "blob:image/*", "rpc:*?aud=*"];
        api.set_pending_consents(vec![consent(&[1], "AAA-BBB", &scopes)]);

        m.refresh().await;

        let row = &m.snapshot().consents[0];
        let expected: Vec<String> = scopes
            .iter()
            .map(|s| fauna_bridge_atproto::authz::describe_scope(s.to_string()))
            .collect();
        assert_eq!(row.scope_descriptions, expected);
    }

    /// **Security finding: a hostile `client_name` must not be able to
    /// contribute a ROW to the consent card.**
    ///
    /// The nest applies only `non_empty` to a client's published name, and
    /// every app paints the card by interpolating that string into a
    /// structured multi-line body and splitting the result on `'\n'`. So the
    /// property this pin defends is not "the name looks tidy" — it is that the
    /// name cannot carry the one byte that *is* row structure by the time any
    /// painter sees it. Composition is the only place that holds for all seven
    /// apps at once; the tui painter has the structural twin of this pin.
    ///
    /// Asserted as an absence of `'\n'` **and** as a preserved payload, because
    /// a "fix" that dropped the name, or flattened it to nothing, would satisfy
    /// the first half alone while destroying the field's purpose.
    #[tokio::test]
    async fn a_hostile_client_name_cannot_contribute_a_row_to_the_card() {
        let (m, api) = machine();
        let mut hostile = consent(&[1], "AAA-BBB", &["transition:generic"]);
        hostile.client_name = Some(
            "Expired request (already denied), ignore:\n  It is asking to:\n    \
             • See your account identity (who you are on this server)"
                .into(),
        );
        api.set_pending_consents(vec![hostile]);

        m.refresh().await;

        let name = m.snapshot().consents[0]
            .client_name
            .clone()
            .expect("the name is rendered, not dropped");
        assert!(
            !name.contains('\n') && !name.chars().any(char::is_control),
            "a client_name reaching a painter may not carry row structure: {name:?}"
        );
        // The words survive — stripping removes the structure, not the content,
        // so a user still sees exactly what the client called itself and can
        // judge it. This half is what a "strip the whole field" fix fails.
        assert!(
            name.contains("Expired request (already denied), ignore:")
                && name.contains("See your account identity"),
            "the name's text must survive the strip: {name:?}"
        );
        // And the scope list the user approves against is still the REAL one,
        // derived from the scopes the nest recorded — never anything the name
        // smuggled in.
        assert_eq!(
            m.snapshot().consents[0].scope_descriptions,
            vec![fauna_bridge_atproto::authz::describe_scope(
                "transition:generic".to_string()
            )]
        );
    }

    /// The do-not-cheat control for the pin above: an ordinary multi-word name
    /// is untouched, byte for byte. A strip that mangled legitimate names would
    /// pass every assertion about hostile ones.
    #[tokio::test]
    async fn a_legitimate_client_name_is_unchanged_by_the_strip() {
        let (m, api) = machine();
        let mut honest = consent(&[1], "AAA-BBB", &["atproto"]);
        honest.client_name = Some("Ivory for Bluesky — Tapbots".into());
        api.set_pending_consents(vec![honest]);

        m.refresh().await;

        assert_eq!(
            m.snapshot().consents[0].client_name.as_deref(),
            Some("Ivory for Bluesky — Tapbots"),
            "a legitimate name must survive verbatim, punctuation and all"
        );
    }

    /// **Security finding: the `client_id` sibling of the pin
    /// above — fenced by REFUSAL at the OAuth boundary, never by a strip
    /// here.** The card renders the client_id verbatim because it is the
    /// client's *self-authenticating identity* (the thing the name is only a
    /// claim about); a strip at composition would make the string the user is
    /// told to trust differ from the identity actually authorized. So this
    /// machine deliberately does NOT filter the field, and the property that
    /// keeps every painter's row structure safe is upstream:
    /// `plan_client_id` refuses any control character in BOTH arms, so a
    /// row-forging client_id never becomes a PAR, a consent row, or a grant.
    /// This pin holds the coupling: if that boundary weakened, it reddens
    /// here — in the crate that owns the card's composition — not only in the
    /// bridge crate's own tests.
    #[tokio::test]
    async fn a_hostile_client_id_never_becomes_a_consent_row() {
        // The exact end-to-end probe: the newline rides INSIDE
        // the `scope` value, so the unknown-parameter refusal never sees it
        // and `split_whitespace` still finds the base scope.
        let probe = "http://localhost?scope=atproto\nIt is asking to:";
        assert!(
            matches!(
                fauna_bridge_atproto::oauth_client::plan_client_id(probe.to_string()),
                fauna_bridge_atproto::oauth_client::ClientIdPlan::Deny { .. }
            ),
            "the boundary must refuse the row-forging client_id before it can reach a card"
        );
        // And the machine's own posture, stated as an assertion so a future
        // "helpful" strip is caught: a client_id the boundary DID admit
        // reaches the snapshot byte for byte.
        let (m, api) = machine();
        let admitted = "http://localhost?scope=atproto&redirect_uri=http://127.0.0.1/cb";
        let mut row = consent(&[1], "AAA-BBB", &["atproto"]);
        row.client_id = admitted.to_string();
        api.set_pending_consents(vec![row]);
        m.refresh().await;
        assert_eq!(
            m.snapshot().consents[0].client_id,
            admitted,
            "an admitted identity renders verbatim — refusal upstream, never a strip here"
        );
    }

    /// Approving sends the approval, consumes the request, and leaves no card
    /// behind — the re-list is part of the gesture, not something the page has
    /// to remember to do.
    #[tokio::test]
    async fn approving_answers_the_nest_and_clears_the_card() {
        let (m, api) = machine();
        api.set_pending_consents(vec![consent(&[0xAB, 0xCD], "ABC-DEF", &["atproto"])]);
        m.refresh().await;

        m.resolve_consent("abcd".into(), true).await;

        assert!(api.calls().contains(&FakeCall::ResolveConsent {
            consent_id: vec![0xAB, 0xCD],
            approved: true,
        }));
        let snap = m.snapshot();
        assert!(
            snap.consents.is_empty(),
            "the answered request must not still render as a card"
        );
        assert!(snap.error.is_none(), "a clean answer clears the page error");
    }

    /// A decline crosses the wire AS a decline. Recording it is what gives the
    /// waiting browser a clean refusal instead of a timeout, so dropping it (or
    /// treating it as a local dismiss) is a real behavior loss, not a shortcut.
    #[tokio::test]
    async fn declining_is_recorded_not_silently_dismissed() {
        let (m, api) = machine();
        api.set_pending_consents(vec![consent(&[9], "XYZ-123", &["atproto"])]);
        m.refresh().await;

        m.resolve_consent("09".into(), false).await;

        assert!(api.calls().contains(&FakeCall::ResolveConsent {
            consent_id: vec![9],
            approved: false,
        }));
        assert!(api.pending_consents().is_empty());
        assert!(m.snapshot().consents.is_empty());
    }

    /// `resolved: false` is the one answer nest gives for "already answered /
    /// expired / not yours", and the page says so rather than guessing which.
    ///
    /// The id here matches no live row, so the fake's own `WHERE`-clause model
    /// is what produces the `false` — the same way nest does. An implementation
    /// that treated `Ok(false)` as success would leave the error clear and
    /// redden this.
    #[tokio::test]
    async fn answering_a_request_that_is_no_longer_live_reports_it() {
        let (m, api) = machine();
        api.set_pending_consents(vec![consent(&[1], "AAA-BBB", &["atproto"])]);
        m.refresh().await;

        m.resolve_consent("ff".into(), true).await;

        let snap = m.snapshot();
        assert!(
            snap.error.is_some(),
            "an unmatched answer must surface on error-message, never pass silently"
        );
        assert!(
            !snap.consents.is_empty(),
            "the re-list runs on this path too — the still-live request stays on screen"
        );
    }

    /// A failed list keeps the rows already painted. A card the user can still
    /// compare a code against beats a panel that blanks itself the moment a
    /// poll hiccups — and the page error says the read failed.
    #[tokio::test]
    async fn a_failed_list_keeps_the_rows_already_on_screen() {
        let (m, api) = machine();
        api.set_pending_consents(vec![consent(&[1], "AAA-BBB", &["atproto"])]);
        m.refresh().await;
        assert_eq!(m.snapshot().consents.len(), 1);

        api.fail_list_consents(AtprotoSettingsApiError::Transient {
            detail: "socket closed".into(),
        });
        m.refresh().await;

        let snap = m.snapshot();
        assert_eq!(snap.consents.len(), 1, "the prior rows survive the failure");
        assert!(snap.error.is_some());
    }

    /// An unassigned request (a PAR that carried no `login_hint`) is listed to
    /// every account on this nest and renders exactly like an account's own —
    /// which is precisely why the binding code is unique among every LIVE row
    /// rather than per account.
    #[tokio::test]
    async fn an_unassigned_request_renders_like_any_other() {
        let (m, api) = machine();
        let mut unassigned = consent(&[2], "QRS-TUV", &["atproto"]);
        unassigned.client_name = None;
        api.set_pending_consents(vec![unassigned]);

        m.refresh().await;

        let row = &m.snapshot().consents[0];
        assert_eq!(row.code, "QRS-TUV");
        assert_eq!(
            row.client_name, None,
            "no resolved name is a real state, not an error — the card falls back to client_id"
        );
        assert!(!row.client_id.is_empty());
    }

    // ── The consent-time grant (third-party-kinds.md § The record doors) ──

    const HOLDER: [u8; 32] = [9; 32];

    /// A records request from `https://app.example.com/…` whose document
    /// declares two kinds, with both keys attested.
    fn records_consent(id: &[u8]) -> NestConsentRow {
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
        let did = fauna_protocol::kind_manifest::ed25519_did_key(&key.verifying_key().to_bytes());
        let kinds: Vec<_> = ["ext.app.example.com.notes", "ext.app.example.com.todo"]
            .iter()
            .map(|k| {
                serde_json::json!({
                    "kind": k, "class": "state", "merge": "latest-wins", "floor": "none"
                })
            })
            .collect();
        let payload = serde_json::json!({
            "version": 1,
            "publisher": { "domain": "app.example.com", "key": did },
            "kinds": kinds,
        });
        let mut row = consent(
            id,
            "REC-ORD",
            &["atproto", "fauna:records:rw:ext.app.example.com.*"],
        );
        row.holder_x25519 = Some(HOLDER.to_vec());
        row.writer_ed25519 = Some(vec![0x5A; 32]);
        row.fauna_manifest = Some(fauna_protocol::kind_manifest::sign_manifest(
            &key, &payload, None,
        ));
        row
    }

    struct GrantFixture {
        manifests: Arc<fauna_client_config::test_helpers::FakeKindManifestStore>,
        ledger: Arc<fauna_client_config::test_helpers::FakeSuccessionLedgerStore>,
    }

    fn wire_grants(m: &AtprotoSettingsMachine) -> GrantFixture {
        let owner = fauna_core::identity::ActorKeypair::generate();
        let manifests = Arc::new(fauna_client_config::test_helpers::FakeKindManifestStore::empty());
        let ledger = Arc::new(
            fauna_client_config::test_helpers::FakeSuccessionLedgerStore::empty(owner.actor_id()),
        );
        m.set_consent_grant_seams(Arc::new(ConsentGrantSeams::from_keypair(
            &owner,
            manifests.clone(),
            ledger.clone(),
        )));
        GrantFixture { manifests, ledger }
    }

    fn grant_calls(api: &FakeAtprotoSettingsNestApi) -> Vec<&'static str> {
        api.calls()
            .iter()
            .filter_map(|c| match c {
                FakeCall::MintGrant { .. } => Some("mint"),
                FakeCall::ResolveConsent { .. } => Some("resolve"),
                FakeCall::RevokeGrant { .. } => Some("revoke"),
                _ => None,
            })
            .collect()
    }

    /// **Mint before resolve.** Approving a records request publishes the
    /// app's manifest row, records the signed `Mint` in the grant log, and
    /// deposits the grant BEFORE the resolve — the nest mints the app's row at
    /// `/oauth/token` with no client in the loop, so the grant must already
    /// rest when it does.
    #[tokio::test]
    async fn approving_a_records_request_deposits_its_grant_before_resolving() {
        let (m, api) = machine();
        let g = wire_grants(&m);
        api.set_pending_consents(vec![records_consent(&[0xAB])]);
        m.refresh().await;

        m.resolve_consent("ab".into(), true).await;

        assert_eq!(grant_calls(&api), ["mint", "resolve"]);
        assert!(m.snapshot().error.is_none(), "{:?}", m.snapshot().error);
        assert!(
            g.manifests
                .row("https://app.example.com/client-metadata.json")
                .is_some(),
            "the manifest row is published, so the account's replicas admit the kinds"
        );
        let events = g.ledger.current().grant_events;
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].kind,
            fauna_core::grant_event::GrantEventKind::Mint
        );
        assert_eq!(events[0].holder.as_slice(), HOLDER.as_slice());
    }

    /// The card says what a records approve grants: the publisher, how many
    /// kinds the verified manifest declares under the wildcard, that keys to
    /// exactly those records are shared — and "read only" when the app
    /// attested no writer key (`authorization-server.md` § Scope grammar →
    /// *The third arm*).
    #[tokio::test]
    async fn a_records_scope_card_row_names_the_publisher_and_kind_count() {
        let (m, api) = machine();
        let mut read_only = records_consent(&[0xCD]);
        read_only.writer_ed25519 = None;
        api.set_pending_consents(vec![records_consent(&[0xAB]), read_only]);
        m.refresh().await;

        let snap = m.snapshot();
        let rw = &snap.consents[0].scope_descriptions[1];
        assert!(
            rw.starts_with("Read and write 2 kinds of records published by app.example.com"),
            "{rw}"
        );
        let ro = &snap.consents[1].scope_descriptions[1];
        assert!(ro.contains("read only"), "{ro}");
        assert!(
            ro.contains("2 kinds of records published by app.example.com"),
            "{ro}"
        );
    }

    /// An app that has not wired the grant's seams refuses the approve rather
    /// than resolving it keyless — the app would be connected with no access
    /// to the records the card promised. Nothing crosses the wire.
    #[tokio::test]
    async fn an_unwired_machine_refuses_a_records_approve_and_resolves_nothing() {
        let (m, api) = machine();
        api.set_pending_consents(vec![records_consent(&[0xAB])]);
        m.refresh().await;

        m.resolve_consent("ab".into(), true).await;

        assert!(grant_calls(&api).is_empty());
        assert!(m.snapshot().error.is_some());
        assert_eq!(
            api.pending_consents().len(),
            1,
            "the request stays answerable"
        );
    }

    /// A declined records request mints nothing — on a wired machine too.
    #[tokio::test]
    async fn declining_a_records_request_mints_nothing() {
        let (m, api) = machine();
        let g = wire_grants(&m);
        api.set_pending_consents(vec![records_consent(&[0xAB])]);
        m.refresh().await;

        m.resolve_consent("ab".into(), false).await;

        assert_eq!(grant_calls(&api), ["resolve"]);
        assert!(g.ledger.current().grant_events.is_empty());
    }

    /// A deposited grant whose resolve does not land (another device answered
    /// first) is withdrawn again — the nest first, then a signed `Revoke` — so
    /// it never outlives the consent it was minted for.
    #[tokio::test]
    async fn a_grant_whose_consent_did_not_resolve_is_withdrawn() {
        let (m, api) = machine();
        let g = wire_grants(&m);
        api.set_pending_consents(vec![records_consent(&[0xAB])]);
        m.refresh().await;
        api.resolve_answers_gone();

        m.resolve_consent("ab".into(), true).await;

        assert_eq!(grant_calls(&api), ["mint", "resolve", "revoke"]);
        let mut kinds: Vec<_> = g
            .ledger
            .current()
            .grant_events
            .iter()
            .map(|e| e.kind as u8)
            .collect();
        kinds.sort_unstable();
        assert_eq!(
            kinds,
            [
                fauna_core::grant_event::GrantEventKind::Mint as u8,
                fauna_core::grant_event::GrantEventKind::Revoke as u8
            ]
        );
        assert!(m.snapshot().error.is_some(), "the gone line still shows");
    }

    /// A refused deposit leaves the request unanswered.
    #[tokio::test]
    async fn a_refused_deposit_does_not_resolve() {
        let (m, api) = machine();
        wire_grants(&m);
        api.set_pending_consents(vec![records_consent(&[0xAB])]);
        m.refresh().await;
        api.fail_mint_grant(AtprotoSettingsApiError::Transient {
            detail: "quota".into(),
        });

        m.resolve_consent("ab".into(), true).await;

        assert_eq!(grant_calls(&api), ["mint"]);
        assert_eq!(api.pending_consents().len(), 1);
        assert!(m.snapshot().error.is_some());
    }

    /// `poll_pending_consents` is what a visible page's backstop poll calls —
    /// it must pick up a request that arrived AFTER the page's initial load,
    /// the exact case a fresh navigation's `refresh()` cannot cover (an
    /// unassigned request fans out to nobody, so nothing else nudges an
    /// already-mounted page to re-list).
    #[tokio::test]
    async fn poll_picks_up_a_request_that_arrived_after_the_initial_load() {
        let (m, api) = machine();
        m.refresh().await;
        assert!(m.snapshot().consents.is_empty(), "nothing pending yet");

        api.set_pending_consents(vec![consent(&[7], "POL-LED", &["atproto"])]);
        m.poll_pending_consents().await;

        let snap = m.snapshot();
        assert_eq!(snap.consents.len(), 1);
        assert_eq!(snap.consents[0].code, "POL-LED");
    }

    /// The poll notifies on every call, even a no-op tick with nothing new —
    /// a SwiftUI page only re-renders (and so only re-discovers a new card)
    /// on the observer firing, so a poll that skipped notifying when nothing
    /// looked different would never surface the row it just fetched.
    #[tokio::test]
    async fn poll_notifies_the_observer_every_call() {
        let api = Arc::new(FakeAtprotoSettingsNestApi::new());
        let observer = CountingObserver::new();
        let verifier = crate::custody::FakeGenesisVerifier::new(Err(
            crate::custody::VerifyFailure::Fetch("test default: directory unreachable".into()),
        ));
        let m = AtprotoSettingsMachine::new(observer.clone(), api, verifier, None);
        m.refresh().await;
        let before = observer.count();

        m.poll_pending_consents().await;

        assert!(
            observer.count() > before,
            "a poll tick must repaint even when nothing changed"
        );
    }
}

/// The 72 h recovery-fork contest (`atproto-pds-bridge.md` § State & data
/// shape, the recovery-fork contest).
///
/// What these pin, in one sentence each: the card is composed from the
/// directory's own log and nothing else; the pass NEVER signs without a
/// consent scoped to the violation standing right now; the gesture cannot be
/// aimed at a different op; and a completed contest leaves nothing that could
/// authorize a second one.
#[cfg(test)]
mod contest_tests {
    use super::*;
    use crate::contest::{FakeContestActor, FakeContestLog};
    use crate::nest_api::FakeAtprotoSettingsNestApi;
    use crate::observer::CountingObserver;
    use fauna_client_atproto::identity_store::InMemoryAtprotoIdentityStore;

    const DID: &str = "did:plc:abc123contested";
    const CONTESTED: &str = "bafycontestedop";
    const FORK_POINT: &str = "bafyforkpoint";

    struct Setup {
        machine: Arc<AtprotoSettingsMachine>,
        actor: Arc<FakeContestActor>,
        custody: InMemoryAtprotoIdentityStore,
        verifier: Arc<crate::custody::FakeGenesisVerifier>,
    }

    async fn setup(log: FakeContestLog) -> Setup {
        let api = Arc::new(FakeAtprotoSettingsNestApi::new());
        let custody = InMemoryAtprotoIdentityStore::default();
        mint_rotation_key(&custody, 1)
            .await
            .expect("seed senior key");
        let actor = FakeContestActor::new();
        actor.set_log(log);
        // The contest pass answers a CONFIRMED custody alarm, so every fixture
        // here stands on one: the seniority check reports a box key senior
        // over a standing op.
        let verifier = crate::custody::FakeGenesisVerifier::new(Ok(SeniorityVerdict::Mismatch(
            crate::custody::MismatchReason::SeniorKeyDiffers {
                standing_index: 1,
                found: Some("did:key:zQ3shBoxJuniorKey".into()), // gitleaks:allow
            },
        )));
        let machine = AtprotoSettingsMachine::new_with_seams(
            CountingObserver::new(),
            api.clone(),
            // The contest pass answers a CONFIRMED custody alarm, so every
            // fixture here stands on one: the seniority check reports a
            // box key senior over a standing op.
            verifier.clone(),
            None,
            None,
            crate::retirement::FakeTombstoneActor::new(),
            actor.clone(),
        );
        machine.set_identity_store(Arc::new(custody.clone()));
        machine.set_credential_store(Arc::new(crate::credentials::FakeCredentialStore::new()));
        api.set_status(
            "hosted_visible",
            true,
            "example.com",
            "alice.example.com",
            Some(NestIdentitySummary {
                handle: "alice.example.com".into(),
                method: "plc".into(),
                status: "active".into(),
                did: Some(DID.into()),
                ..Default::default()
            }),
        );
        Setup {
            machine,
            actor,
            custody,
            verifier,
        }
    }

    fn contestable() -> FakeContestLog {
        FakeContestLog::Contestable {
            contested_op_cid: CONTESTED.into(),
            fork_point_cid: FORK_POINT.into(),
        }
    }

    /// Drive the machine to a CONFIRMED custody alarm — two convergences, per
    /// the seniority check's one-sighting debounce. Every contest behaviour
    /// below starts from here, because that is the only state the contest pass
    /// runs in at all.
    async fn alarmed(s: &Setup) {
        s.machine.refresh().await;
        s.machine.refresh().await;
    }

    async fn stored_intents(s: &Setup) -> Vec<(String, String)> {
        s.custody
            .current()
            .contest_intents
            .into_iter()
            .map(|i| (i.did, i.contested_op_cid))
            .collect()
    }

    // ── The card renders off the directory, and only the directory ──────────

    #[tokio::test]
    async fn a_standing_violation_raises_the_contest_card() {
        let s = setup(contestable()).await;
        alarmed(&s).await;

        let card = s.machine.snapshot().contest.expect("the card renders");
        assert_eq!(card.state, "contestable");
        assert!(
            card.show_contest,
            "the button renders only where acting works"
        );
        assert!(
            card.deadline.is_some(),
            "an advisory countdown accompanies a live window"
        );
    }

    /// The overwhelmingly common state: no card at all. A settings page must
    /// not carry a compromise notice it cannot justify.
    #[tokio::test]
    async fn a_clean_log_raises_no_card() {
        let s = setup(FakeContestLog::Clean).await;
        alarmed(&s).await;
        assert!(s.machine.snapshot().contest.is_none());
    }

    /// Unreadable ≠ uncontested. A directory this client cannot reach is a
    /// quiet retry, never a card claiming a state we did not observe.
    #[tokio::test]
    async fn an_unreachable_directory_raises_no_card() {
        let s = setup(FakeContestLog::Unreachable).await;
        alarmed(&s).await;
        assert!(s.machine.snapshot().contest.is_none());
        assert!(s.actor.converge_calls().is_empty(), "and signs nothing");
    }

    /// Decision 2: the honest no-remedy states render as such — a state, a
    /// detail line, and NO button — rather than as a control that cannot work.
    #[tokio::test]
    async fn hopeless_states_render_honestly_and_offer_no_button() {
        for (log, expected_state) in [
            (
                FakeContestLog::NotContestable(ContestEligibility::GenesisViolation),
                "not-contestable",
            ),
            (
                FakeContestLog::NotContestable(ContestEligibility::WindowClosed),
                "window-closed",
            ),
            // A log that does not authenticate — the hostile-directory / MITM
            // case. It is a no-remedy state, NOT a read failure, so it renders
            // like the others: the alarm has already fired, and this surface is
            // the only place that can explain why nothing can be undone. The
            // close returned it as an error instead, which left this
            // card blank; that regression is what this row pins.
            (
                FakeContestLog::NotContestable(ContestEligibility::Unauthenticated(
                    fauna_client_atproto::plc_chain::ChainFailure::BadSignature { index: 1 },
                )),
                "not-contestable",
            ),
        ] {
            let s = setup(log).await;
            alarmed(&s).await;
            let card = s.machine.snapshot().contest.expect("a card still renders");
            assert_eq!(card.state, expected_state);
            assert!(!card.show_contest, "no dead button on {expected_state}");
            assert!(
                card.deadline.is_none(),
                "no countdown on a state the user cannot act on"
            );
        }
    }

    /// The three no-remedy states must not share one copy: "there is no earlier
    /// state to return to" and "the record itself does not check out" call for
    /// different actions from the user, and both render under the same
    /// `not-contestable` state value, so the DETAIL line is the only thing
    /// distinguishing them.
    #[tokio::test]
    async fn each_no_remedy_state_carries_its_own_explanation() {
        let mut seen = std::collections::HashSet::new();
        for eligibility in [
            ContestEligibility::GenesisViolation,
            ContestEligibility::WindowClosed,
            ContestEligibility::Unauthenticated(
                fauna_client_atproto::plc_chain::ChainFailure::CidMismatch { index: 1 },
            ),
        ] {
            let s = setup(FakeContestLog::NotContestable(eligibility.clone())).await;
            alarmed(&s).await;
            let card = s.machine.snapshot().contest.expect("a card still renders");
            assert!(
                seen.insert(format!("{:?}", card.detail)),
                "{eligibility:?} reuses another state's copy: {:?}",
                card.detail
            );
        }
    }

    /// The card obeys the alarm's own debounce: a custody contradiction seen
    /// ONCE raises no contest surface. A first sighting has a legitimate cause
    /// (a sibling's fresh re-mint key not yet synced), and a contest card that
    /// appeared and then vanished would teach the user to ignore the real one —
    /// the same reasoning that keeps the alarm itself quiet for one pass.
    #[tokio::test]
    async fn a_first_sighting_raises_no_contest_card() {
        let s = setup(contestable()).await;
        s.machine.refresh().await;
        assert!(s.machine.snapshot().contest.is_none());
        assert_eq!(
            s.actor.plan_calls(),
            0,
            "and the directory is not even asked until the alarm is confirmed"
        );

        s.machine.refresh().await;
        assert!(s.machine.snapshot().contest.is_some());
    }

    /// While custody holds, the contest pass costs nothing at all — no plan
    /// call, so no second request to the public PLC directory on every refresh
    /// of every healthy identity.
    #[tokio::test]
    async fn a_healthy_identity_never_touches_the_directory_for_a_contest() {
        let s = setup(contestable()).await;
        s.verifier.set_verdict(Ok(SeniorityVerdict::Verified {
            standing_ops: 2,
            observed_seniors: vec![],
        }));
        s.machine.refresh().await;
        s.machine.refresh().await;
        assert_eq!(s.actor.plan_calls(), 0);
        assert!(s.machine.snapshot().contest.is_none());
    }

    // ── The pass is an executor of consent, never its author ────────────────

    /// Decision 6, iron-clad: detection is automatic, contesting is not. A
    /// convergence that finds a live, contestable violation and no consent
    /// composes the card and stops.
    #[tokio::test]
    async fn a_convergence_never_contests_on_its_own() {
        let s = setup(contestable()).await;
        alarmed(&s).await;
        s.machine.refresh().await;

        assert!(
            s.actor.converge_calls().is_empty(),
            "no consent, no signature — an auto-contester is a detector promoted to an actor"
        );
        assert_eq!(stored_intents(&s).await, Vec::new());
        assert_eq!(s.actor.log(), contestable(), "the log is untouched");
    }

    /// **Everything a hostile box can arrange, and still nothing is signed.**
    ///
    /// The box controls the nest's status reply (so it names the DID, the
    /// handle and the identity's whole state) and it authored the violating op
    /// itself. What it cannot author is the user's consent record, which lives
    /// only in the account plane's custody behind the client-side sole writer — so the whole
    /// arrangement yields a card and no signature.
    #[tokio::test]
    async fn everything_the_box_can_arrange_still_signs_nothing() {
        let s = setup(contestable()).await;
        // The box also reports a *different* op as contested than any consent
        // could name — the "different attack" arm of decision 7 — and drives
        // several convergences hoping one of them acts.
        alarmed(&s).await;
        s.actor.set_log(FakeContestLog::Contestable {
            contested_op_cid: "bafySOMEOTHEROP".into(),
            fork_point_cid: FORK_POINT.into(),
        });
        s.machine.refresh().await;
        s.machine.refresh().await;

        assert!(s.actor.converge_calls().is_empty());
        assert_eq!(stored_intents(&s).await, Vec::new());
    }

    /// A consent for op X does not authorize contesting op Y. The pair
    /// `(did, cid)` is the scope, and it is re-checked against the log's
    /// CURRENT first violation every pass — so a completed contest cannot be
    /// replayed and a *new* hostile op needs a new human decision.
    #[tokio::test]
    async fn a_consent_for_one_op_does_not_authorize_another() {
        let s = setup(contestable()).await;
        let store = s.custody.clone();
        record_contest_intent(&store, DID, "bafyA_DIFFERENT_OP", 1)
            .await
            .unwrap();

        alarmed(&s).await;

        assert!(
            s.actor.converge_calls().is_empty(),
            "the machine does not even call converge for an unconsented violation"
        );
        assert_eq!(s.actor.log(), contestable(), "nothing was contested");
    }

    /// The seam's own gate is independent of the machine's: even handed the
    /// call directly, a converge whose intents do not cover the standing
    /// violation refuses. Two gates, because the one that licenses a signature
    /// must sit next to the signing.
    #[tokio::test]
    async fn the_seam_refuses_an_unconsented_violation_too() {
        let s = setup(contestable()).await;
        let progress = s
            .actor
            .converge(
                DID.into(),
                vec![],
                vec![fauna_core::data::AtprotoContestIntent {
                    did: DID.into(),
                    contested_op_cid: "bafyELSEWHERE".into(),
                    requested_at: 1,
                }],
                0,
            )
            .await
            .unwrap();
        assert_eq!(progress, ContestProgress::NoConsentForThisViolation);
    }

    // ── The gesture ─────────────────────────────────────────────────────────

    /// The confirm records consent scoped to the op the card described, then
    /// converges — and the fork lands.
    #[tokio::test]
    async fn the_confirm_records_the_scoped_consent_and_contests() {
        let s = setup(contestable()).await;
        alarmed(&s).await;

        s.machine.request_contest().await;

        assert_eq!(
            stored_intents(&s).await,
            vec![(DID.to_string(), CONTESTED.to_string())],
            "consent is recorded for exactly the op the card named"
        );
        let calls = s.actor.converge_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].did, DID);
        assert!(
            !calls[0].held_keys.is_empty(),
            "the whole stored ring crosses the seam — signing is what it does"
        );
        assert_eq!(
            s.actor.log(),
            FakeContestLog::Clean,
            "the contested suffix is nullified"
        );
    }

    /// With no card there is no op to scope a consent to, so the gesture
    /// refuses and records nothing. A consent against nothing would be the
    /// standing "always contest" authorization decision 6 forbids.
    #[tokio::test]
    async fn the_confirm_refuses_when_no_violation_stands() {
        let s = setup(FakeContestLog::Clean).await;
        alarmed(&s).await;

        s.machine.request_contest().await;

        assert!(s.machine.snapshot().error.is_some());
        assert_eq!(stored_intents(&s).await, Vec::new());
        assert!(s.actor.converge_calls().is_empty());
    }

    /// A failed submit leaves the consent standing and the log untouched, so
    /// the next convergence simply tries again — the crash-safety property,
    /// with no client-side "already contested" flag to get out of sync.
    #[tokio::test]
    async fn a_failed_submit_retries_on_the_next_convergence() {
        let s = setup(contestable()).await;
        alarmed(&s).await;
        s.actor.set_submit_fails(Some("directory 503".into()));

        s.machine.request_contest().await;
        assert_eq!(s.actor.log(), contestable(), "nothing landed");
        assert_eq!(
            stored_intents(&s).await,
            vec![(DID.to_string(), CONTESTED.to_string())],
            "the consent survives the failure — that is what makes the retry free"
        );

        s.actor.set_submit_fails(None);
        s.machine.refresh().await;
        assert_eq!(
            s.actor.log(),
            FakeContestLog::Clean,
            "the very next convergence finishes it, with no new gesture"
        );
    }

    /// A sibling device completes off the synced consent: this device never
    /// gestured, but the intent arrived through account-plane sync and the
    /// convergence finishes the job inside the window.
    #[tokio::test]
    async fn a_sibling_device_completes_off_the_synced_consent() {
        let s = setup(contestable()).await;
        let store = s.custody.clone();
        record_contest_intent(&store, DID, CONTESTED, 1)
            .await
            .unwrap();

        alarmed(&s).await;

        assert_eq!(s.actor.converge_calls().len(), 1);
        assert_eq!(s.actor.log(), FakeContestLog::Clean);
    }

    /// After the fork lands, the intent is spent by construction: the log has
    /// no violation, so a later convergence contests nothing — the record is
    /// inert forever rather than a standing authorization.
    #[tokio::test]
    async fn a_spent_consent_authorizes_nothing_afterwards() {
        let s = setup(contestable()).await;
        alarmed(&s).await;
        s.machine.request_contest().await;
        let after_contest = s.actor.converge_calls().len();

        // A NEW hostile op appears. The old consent named a different CID, so
        // it authorizes nothing here — this needs a fresh human decision.
        s.actor.set_log(FakeContestLog::Contestable {
            contested_op_cid: "bafyA_SECOND_ATTACK".into(),
            fork_point_cid: FORK_POINT.into(),
        });
        s.machine.refresh().await;

        assert_eq!(
            s.actor.converge_calls().len(),
            after_contest,
            "the spent consent does not carry over to the next attack"
        );
        assert_eq!(
            s.machine.snapshot().contest.expect("a fresh card").state,
            "contestable",
            "…the surface re-presents for a new gesture instead"
        );
    }

    /// The card and the alarm come down on the DIRECTORY's evidence, not on
    /// our own submit: believing ourselves would report a recovery the network
    /// has not accepted.
    #[tokio::test]
    async fn the_card_clears_only_once_the_log_agrees() {
        let s = setup(contestable()).await;
        alarmed(&s).await;
        s.machine.request_contest().await;

        // The submit landed, but this pass does not clear the card itself.
        assert!(s.machine.snapshot().contest.is_some());

        // The next convergence reads the nullified log and takes it down.
        s.machine.refresh().await;
        assert!(s.machine.snapshot().contest.is_none());
    }

    // ── The ceremony (slice B): the confirm card lives HERE, not per app ────
    //
    // The open/cancel state is machine-side on purpose: seven apps must run one
    // ceremony, not seven. These pin the states a page renders and, more
    // importantly, the two a page must never be able to reach — a confirm
    // surface over a violation that cannot be fought, and a second signature
    // from a second press.

    #[tokio::test]
    async fn opening_the_confirm_card_composes_the_three_things_it_must_name() {
        let s = setup(contestable()).await;
        alarmed(&s).await;

        assert!(
            s.machine.snapshot().contest_confirm.is_none(),
            "the ceremony is closed until the user opens it"
        );
        s.machine.open_contest_confirm();

        let card = s
            .machine
            .snapshot()
            .contest_confirm
            .expect("the confirm card opens");
        let keys: Vec<&str> = card.lines.iter().map(|l| l.key.as_str()).collect();
        // `ui/atproto.md` § User actions: the card names the op being
        // contested, what the fork will sign, and the deadline.
        assert!(
            keys.contains(&"atproto_settings.contest_confirm_undo"),
            "what is being undone: {keys:?}"
        );
        assert!(
            keys.contains(&"atproto_settings.contest_confirm_signs"),
            "what this device signs: {keys:?}"
        );
        assert!(
            keys.iter()
                .any(|k| k.starts_with("atproto_settings.contest_deadline")),
            "the deadline: {keys:?}"
        );
        assert!(!card.in_progress);
        assert!(
            !stored_intents(&s).await.iter().any(|(d, _)| d == DID),
            "opening the card signs nothing and consents to nothing"
        );
    }

    /// Decision 2's dead-button rule, enforced at the machine rather than by
    /// seven pages remembering to hide a control: a state that cannot be fought
    /// cannot open a surface whose only button fights it.
    #[tokio::test]
    async fn a_hopeless_state_cannot_open_the_confirm_card() {
        let s = setup(FakeContestLog::NotContestable(
            ContestEligibility::WindowClosed,
        ))
        .await;
        alarmed(&s).await;
        assert_eq!(
            s.machine.snapshot().contest.expect("a card").state,
            "window-closed"
        );

        s.machine.open_contest_confirm();

        assert!(
            s.machine.snapshot().contest_confirm.is_none(),
            "no confirm surface over a violation that cannot be fought"
        );
        assert!(
            s.machine.snapshot().error.is_some(),
            "and the refusal is loud, never a silent no-op (testing.md rule 11)"
        );
    }

    #[tokio::test]
    async fn cancel_closes_the_ceremony_having_signed_nothing() {
        let s = setup(contestable()).await;
        alarmed(&s).await;
        s.machine.open_contest_confirm();
        let before = s.actor.converge_calls().len();

        s.machine.cancel_contest();

        assert!(s.machine.snapshot().contest_confirm.is_none());
        assert!(
            s.machine.snapshot().contest.is_some(),
            "…but the violation notice itself stays up"
        );
        assert_eq!(s.actor.converge_calls().len(), before, "nothing submitted");
        assert!(
            !stored_intents(&s).await.iter().any(|(d, _)| d == DID),
            "no intent recorded — `ui/atproto.md` § User actions"
        );
    }

    /// The ceremony cannot outlive its subject. Nothing renders a confirm card
    /// over a violation that is no longer standing.
    #[tokio::test]
    async fn the_ceremony_closes_when_the_violation_stops_standing() {
        let s = setup(contestable()).await;
        alarmed(&s).await;
        s.machine.open_contest_confirm();
        assert!(s.machine.snapshot().contest_confirm.is_some());

        s.actor.set_log(FakeContestLog::Clean);
        s.machine.refresh().await;

        assert!(s.machine.snapshot().contest.is_none());
        assert!(
            s.machine.snapshot().contest_confirm.is_none(),
            "the confirm card cannot outlive the card it confirms"
        );
    }

    /// `ui/atproto.md` § User actions: "Failure keeps the card open with
    /// `error-message` populated; retry is safe."
    ///
    /// The ambient converge pass is deliberately QUIET on failure (unreachable
    /// ≠ uncontested, retried next pass). A converge the user just asked for is
    /// not ambient: silence there is a press that did nothing.
    #[tokio::test]
    async fn a_failed_submit_keeps_the_ceremony_open_and_says_so() {
        let s = setup(contestable()).await;
        alarmed(&s).await;
        s.machine.open_contest_confirm();
        s.actor
            .set_submit_fails(Some("directory refused the fork".into()));

        s.machine.request_contest().await;

        let snap = s.machine.snapshot();
        let card = snap
            .contest_confirm
            .expect("the card stays open so the user can retry");
        assert!(!card.in_progress, "and the button is pressable again");
        assert_eq!(
            snap.error.as_ref().map(|e| e.key.as_str()),
            Some("atproto_settings.error_request_contest"),
            "the failure reaches the page's error-message"
        );
        assert!(
            snap.contest.is_some(),
            "the violation still stands — nothing was accepted"
        );
    }

    /// A quiet ambient pass is still quiet: only the gesture reports.
    #[tokio::test]
    async fn the_ambient_pass_stays_quiet_when_the_directory_refuses() {
        let s = setup(contestable()).await;
        let store = s.custody.clone();
        record_contest_intent(&store, DID, CONTESTED, 1)
            .await
            .unwrap();
        s.actor.set_submit_fails(Some("transient".into()));

        alarmed(&s).await;

        assert!(
            s.machine.snapshot().error.is_none(),
            "a background retry must not paint an error the user did not cause"
        );
    }

    /// The ceremony is over on our side once the fork is away — but the notice
    /// stays until the DIRECTORY agrees, which is the invariant
    /// `the_card_clears_only_once_the_log_agrees` pins for the outer card.
    #[tokio::test]
    async fn a_successful_submit_closes_the_ceremony_but_not_the_notice() {
        let s = setup(contestable()).await;
        alarmed(&s).await;
        s.machine.open_contest_confirm();

        s.machine.request_contest().await;

        let snap = s.machine.snapshot();
        assert!(snap.contest_confirm.is_none(), "the user's part is done");
        assert!(
            snap.contest.is_some(),
            "…and we do not report a recovery the network has not accepted"
        );
        assert!(snap.error.is_none());
    }
}
