//! UniFFI façade for the **stolen-identity succession ceremony**
//! (`docs/goal/behavior/identity-succession.md` § Implementation status today,
//! the correction paragraph) — the native driver the three FFI apps
//! (macOS, iOS, windows) owe, over the shared orchestration every other app
//! already runs.
//!
//! **This module composes; it does not sequence.** The whole app-side
//! orchestration lives in `fauna_client_recovery::ceremony`, native-only,
//! carrying the nine-item defect-avoidance contract —
//! each item a defect some app already shipped. tui consumes it directly and
//! web drives the same primitives in the same order over browser transports
//! (`libs/fauna-wasm/src/succession.rs`). What was missing was any route to it
//! at all from this crate: `fauna-ffi` did not depend on
//! `fauna-client-recovery` until 2026-08-21, so apple/windows/android could not
//! reach the ceremony in any form. ⚠ **Copying tui's earlier shape copies the
//! gap** — consume the module, never re-derive its sequencing.
//!
//! ## Why this is ONE export and not five
//!
//! The ceremony's correctness is almost entirely its **order**, and three of
//! its steps are worthless alone:
//!
//! 1. **Persist the successor seed before anything that can fail or block** —
//!    off [`SuccessionAttempt::successor_secret_hex`], which is reachable
//!    without matching precisely so no surface persists on only one arm. At the
//!    instant the ceremony returns, that seed exists nowhere else in the world
//!    and it *is* the account (the *client-only-resident key material* the
//!    no-user-data-loss invariant names by name).
//! 2. **Verify that persist by read-back, never off the store's return** —
//!    `SecretStore::set` is infallible by signature, so a clean return proves
//!    the call was made and nothing more.
//! 3. **Record the succession LINK after the arm resolves** — not beside the
//!    `add_account`. See [`crate::FfiAccountRegistry::record_succession`] for
//!    what silently breaks without it; on the unconfirmed arm the account's move
//!    is not *known* until the reconcile answers, so claiming it beside the seed
//!    would be claiming something unproven.
//!
//! Exporting those as app-callable steps would put the ordering back in every
//! app, which is the shape the whole lift existed to remove. So the app
//! calls [`succession_succeed_with_held_kit`] and renders what comes back.
//!
//! ## The two things the app still supplies
//!
//! * **`db_path_for`** — the per-account MLS store path, as an
//!   [`FfiSuccessorStorePath`] the app implements, never a pre-computed string.
//!   That is deliberate and load-bearing: resolution may *write* (tui's
//!   `account_scope::account_state_dir` creates the scope dir), and the pinned
//!   contract is that **an unreachable nest fails before anything is written**.
//!   Passing an eager path would run that write before the successor's connect;
//!   passing a resolver the ceremony invokes *after* `connect()` makes the eager
//!   shape unrepresentable.
//! * **the render**. Every judgment this surface needs is already shared: the
//!   sweep's arm and its one extra fact come from `SweepStatus::render_view`,
//!   and every non-landed arm's sentence from `ceremony::StolenOutcome::message`
//!   ([`FfiStolenOutcome::message`]). An app resolves keys through its own
//!   pipeline; it never re-derives what happened.
//!
//! The old identity's `MlsEngine` is **not** a parameter — it is read off the
//! [`FfiNestClient`]'s own stashed conversations session
//! ([`FfiNestClient::conversations_engine`]), because MLS holds one engine per
//! `mls_state.db` and an app that handed one in could hand in a second over the
//! same store.
//!
//! Gated behind `recovery-ceremony` (default-on via `store-safe`). Unlike
//! `member-review`'s shape this gates a real dep, so the Go mail-bridge's
//! `--no-default-features` build keeps its checked-in bindings byte-identical.

use std::path::PathBuf;
use std::sync::Arc;

use fauna_client_recovery::ceremony::{
    LandedSuccession, StolenOutcome, SweepStatus, finish_unconfirmed_succession,
    sweep_after_succession,
};
use fauna_client_recovery::linked_fanout::LinkedNestOutcome;
use fauna_client_recovery::linked_fanout::native::NativeLinkedNestDial;
use fauna_client_recovery::{
    RecoveryClient, RecoveryKitStatus, SuccessionAttempt, create_kit, kit_status, parse_kit,
    predecessor_seeds_from_rows, request_seed_alone_replacement_everywhere,
    reseal_escrow_with_held_kit, succeed_with_held_kit, veto_everywhere,
};
use fauna_core::identity::ActorKeypair;

use crate::accounts_registry::FfiAccountRegistry;
use crate::crypto::secret32;
use crate::nest_client::FfiNestClient;
use crate::{FfiError, general_err};

/// The app's per-account MLS store resolver — apple's `AccountStateDir`,
/// windows' `%LOCALAPPDATA%` scope, android's per-account files dir.
///
/// ⚠ **A resolver, not a path.** The ceremony calls this *after* the successor's
/// nest connect succeeds, which is the whole reason it is a callback: an
/// implementation is allowed to create directories, and
/// the pinned ordering is that an unreachable nest fails before anything is
/// written. Handing in a string instead would run that work unconditionally.
///
/// `successor_actor_hex` is the successor identity's 64-char lowercase actor id.
/// The returned path is where the successor's own engine will be opened; it must
/// be a **different** store from the old identity's, since both engines are live
/// at once during the sweep.
#[uniffi::export(with_foreign)]
pub trait FfiSuccessorStorePath: Send + Sync {
    /// Resolve (and, if the app's layout needs it, create/adopt) the successor's
    /// MLS store path.
    fn mls_db_path(&self, successor_actor_hex: String) -> String;
}

/// What the post-succession group sweep managed, as a surface paints it.
///
/// The fields are the shared `SweepStatus::render_view` projection verbatim
/// — the same one web's `LandedSuccessionJs` carries — so apple, windows and web
/// say the same thing about the same outcome rather than each mapping the enum
/// themselves.
///
/// **An app does not paint from `kind` — it hands this to [`sweep_copy`]**
/// (2026-08-27): the lines a surface shows, including
/// the `groups == 0` silence and the two-facts split, are selected by the
/// shared `SweepView::copy`, and `owes_work` is the render gate for
/// `recovery-kit-sweep-retry-button`. Carry this record across the account
/// switch (beside `sweep_state_json` in the app's `SuccessionHandoff`) and call
/// `sweep_copy` at paint time.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiSweepView {
    /// `"no-engine"` / `"failed"` / `"ran"`. A stable token, not a message.
    ///
    /// ⚠ `"no-engine"` is **not** an error. It means conversations were never up
    /// on this device, so there was no engine to sweep from and the groups (if
    /// any) still hold the old leaf — real, reportable, and not a failed
    /// ceremony.
    pub kind: String,
    /// The failure reason on `"failed"`; on `"ran"`, the count of groups that
    /// still owe work, absent when none do. `None` on `"no-engine"`.
    pub detail: Option<String>,
    /// On `"ran"`, how many groups the sweep enumerated; `0` elsewhere.
    pub groups: u32,
    /// On `"ran"`, how many of those the succeeded credential is gone from.
    pub groups_old_leaf_removed: u32,
    /// The size of the roster the sweep can vouch for nothing about — `0` on
    /// every arm that swept nobody.
    pub unattested_members: u32,
    /// Whether the sweep left work a retry could still finish — the shared
    /// `SweepView::owes_work`, the render gate for
    /// `recovery-kit-sweep-retry-button`. Arm-derived, so it needs no
    /// declaration from the app.
    pub owes_work: bool,
}

/// The sweep's own lines — the shared `SweepCopy`, as [`sweep_copy`] selects
/// them. Each is a `LocalizedText` the app resolves through its own pipeline;
/// `None` is a real value (a line the surface must not paint), never an
/// absence to fill in.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiSweepCopy {
    /// What the sweep did, or `None` for a succession over an account with no
    /// groups at all — *"removed from all 0 of your groups"* is reassurance by
    /// vacuity, and the silence is the projection's, not the app's.
    pub outcome: Option<fauna_core::localized::LocalizedText>,
    /// The roster the sweep cannot vouch for, as its OWN line — never a
    /// qualifier folded into the eviction — or `None` when nobody is on it.
    pub unattested: Option<fauna_core::localized::LocalizedText>,
}

/// Select the sweep's lines off the view an app carried across the switch
/// (`settings.md` § Recovery kit → *The sweep's own lines*).
///
/// `renders_retry` declares whether this app paints
/// `recovery-kit-sweep-retry-button` beside the lines on every arm
/// [`FfiSweepView::owes_work`] answers true for. The two degraded arms name
/// that button by label, and a degraded line must never name a control that is
/// not on screen — so an app that has not built the button passes `false` and
/// gets the member-side remedy alone. ⚠ `false` is a parity gap, never a
/// product choice: no FFI app has the button today (`retry_group_sweep` has no
/// FFI face yet), and the flag flips per app as each builds it.
#[uniffi::export]
pub fn sweep_copy(view: FfiSweepView, renders_retry: bool) -> FfiSweepCopy {
    use fauna_client_recovery::ceremony::{SweepRetryAffordance, SweepView};
    let retry = if renders_retry {
        SweepRetryAffordance::Rendered
    } else {
        SweepRetryAffordance::Absent
    };
    let copy = SweepView {
        kind: view.kind,
        detail: view.detail,
        groups: view.groups,
        groups_old_leaf_removed: view.groups_old_leaf_removed,
        unattested_members: view.unattested_members,
    }
    .copy(retry);
    FfiSweepCopy {
        outcome: copy.outcome,
        unattested: copy.unattested,
    }
}

/// What a press of `recovery-kit-sweep-retry-button` answered — the shared
/// [`fauna_client_recovery::ceremony::SweepRetryAnswer`], as a record an app can
/// paint without deciding anything.
///
/// **Three of its five arms are a sentence and nothing else**, which is the
/// point: the button's render gate is unfinished work rather than "this device
/// can retry" (`settings.md` § Recovery kit → *Finishing an unfinished group
/// sweep*), so a device that cannot retry is on screen and must answer in words
/// when pressed. Paint [`Self::message`] on `error-message` — ui.yaml's own note
/// on the id: never construct the outcome app-side.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiSweepRetryAnswer {
    /// `"swept"` / `"no-old-state"` / `"not-landed"` / `"landed-for-another"` /
    /// `"failed"` — a stable token, not a message.
    ///
    /// ⚠ Only `"failed"` is a fault. `"no-old-state"` is the ordinary answer on
    /// a device that holds no conversation history for the retired identity,
    /// and the other two are terminal facts about the chain: nothing was posted
    /// on any of the three, and each sentence names what is left to do.
    pub kind: String,
    /// The sentence to render, as a `LocalizedText` the app resolves through its
    /// own pipeline. `None` on `"swept"` alone, where the outcome renders
    /// through the sweep's own lines ([`sweep_copy`]) instead — a press that
    /// swept must not also say something a second way.
    pub message: Option<fauna_core::localized::LocalizedText>,
    /// On `"swept"`, the fresh view that **replaces** the one the app carried
    /// across the account switch: the retry moved the very facts that view
    /// reports, so re-painting the old one would show the user the state their
    /// press just fixed. `None` on every other arm — nothing swept, so nothing
    /// about the sweep changed.
    pub sweep: Option<FfiSweepView>,
    /// On `"swept"`, the sweep's own account of itself for the e2e state
    /// protocol's `succession_sweep` key — the same JSON
    /// [`FfiLandedSuccession::sweep_state_json`] carries, so a journey asserts
    /// the retry with the shape it already asserts the ceremony with.
    pub sweep_state_json: Option<String>,
    /// On `"swept"`, the people the fresh report can vouch for nothing about —
    /// raw 32-byte actor ids, the exhaustive convention — feeding the same
    /// member-review raise the ceremony's own sweep feeds.
    pub review_roster: Vec<Vec<u8>>,
}

impl From<fauna_client_recovery::ceremony::SweepRetryAnswer> for FfiSweepRetryAnswer {
    fn from(answer: fauna_client_recovery::ceremony::SweepRetryAnswer) -> Self {
        use fauna_client_recovery::ceremony::SweepRetryAnswer;
        let kind = answer.kind().to_string();
        let message = answer.message();
        match answer {
            SweepRetryAnswer::Swept(report) => {
                // Wrapped back into the enum so the three projections below are
                // the SAME ones the ceremony's own outcome goes through — a
                // second mapping here is a second place for the retry and the
                // ceremony to disagree about one sweep.
                let status = SweepStatus::Ran(report);
                FfiSweepRetryAnswer {
                    kind,
                    message,
                    sweep: Some(FfiSweepView::from(&status)),
                    sweep_state_json: Some(status.state_json().to_string()),
                    review_roster: status
                        .review_roster()
                        .iter()
                        .map(|a| a.0.to_vec())
                        .collect(),
                }
            }
            _ => FfiSweepRetryAnswer {
                kind,
                message,
                sweep: None,
                sweep_state_json: None,
                review_roster: Vec::new(),
            },
        }
    }
}

/// `recovery-kit-sweep-retry-button` — finish a sweep the ceremony left
/// unfinished, over the shared
/// [`fauna_client_recovery::ceremony::retry_sweep_as_successor`]
/// (`succession-aftermath.md` § Propagation → *MLS groups*).
///
/// **Never `Err`.** Every arm — including the transport one — comes back as an
/// [`FfiSweepRetryAnswer`] carrying its own sentence, because on this button an
/// answer *is* the gesture's whole product on three arms out of five, and an
/// `FfiError` would reach the app as already-composed English with no arm token
/// beside it. This is the one export in this module shaped that way, and
/// deliberately: [`succession_succeed_with_held_kit`]'s `Err` means "the
/// account did not move", a fact with no user-facing sentence of its own.
///
/// ## What the app supplies
///
/// * `nest` — the **successor's own live session**, i.e. the signed-in one.
///   Unlike the ceremony's sweep this rides the app's existing client rather
///   than building one: the retry runs long after the account switch, so the
///   successor is by definition signed in, and nothing here is revoked.
/// * `accounts` — the registry, for the retired identity's seed. Which
///   predecessor that is (the DIRECT hop, never merely the nearest one whose
///   seed is present) is decided by the shared
///   [`fauna_client_recovery::ceremony::retry_predecessor`], never here.
/// * `old_store_path` / `successor_store_path` — the two MLS stores, as
///   resolvers for [`FfiSuccessorStorePath`]'s own reason: resolution may write.
///   ⚠ They must resolve **different** stores, and `old_store_path` must be the
///   *pure* scope resolution — never one that creates or writes anything.
///   The retired identity must never adopt anything, and the retry turns on
///   whether its store already exists: a resolver that creates one would report
///   a clean run over an empty store while the thief's leaf sits untouched in
///   every real group.
///
/// The live successor engine is read off the client's own stashed conversations
/// session rather than passed in, for [`succession_succeed_with_held_kit`]'s
/// reason: MLS holds one engine per `mls_state.db`, and an app that handed one
/// in could hand in a second over the same store.
#[fauna_uniffi_async::export]
pub async fn succession_retry_group_sweep(
    nest: Arc<FfiNestClient>,
    successor_secret: Vec<u8>,
    accounts: Arc<FfiAccountRegistry>,
    old_store_path: Arc<dyn FfiSuccessorStorePath>,
    successor_store_path: Arc<dyn FfiSuccessorStorePath>,
) -> FfiSweepRetryAnswer {
    retry_group_sweep(
        nest,
        successor_secret,
        accounts,
        old_store_path,
        successor_store_path,
    )
    .await
    .into()
}

/// What an **owed** sweep's unbidden press answers: the press's own answer,
/// plus the report to park in place of the one the app carries.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiOwedSweepAnswer {
    /// The press's answer, exactly as [`succession_retry_group_sweep`] gives
    /// it — its `message` goes on `error-message` as a press's would.
    pub answer: FfiSweepRetryAnswer,
    /// The report to park: the fresh one on `"swept"`, else an arm that still
    /// owes work, so `recovery-kit-sweep-retry-button` renders
    /// (`SweepRetryAnswer::into_owed_status` decides; never an empty report).
    pub parked: FfiSweepView,
    /// `parked` in the e2e state provider's vocabulary — what the app
    /// republishes as `data.succession_sweep`.
    pub parked_state_json: String,
}

/// Discharge the group sweep a **relaunch adoption** owes — the unbidden press
/// of `recovery-kit-sweep-retry-button` that `succession-propagation.md`
/// § Propagation → *Own device fleet* (the relaunch-adoption clause) rules,
/// run from the successor's first authenticated session.
///
/// The same ceremony as [`succession_retry_group_sweep`], same arguments and
/// same never-`Err` contract; it differs only in what it hands back. A press
/// replaces the carried report only when it swept, because the report it
/// carries already owes work; an adoption carries **no** report, so every
/// answer must park one — and which one is shared Rust's call, not the app's.
#[fauna_uniffi_async::export]
pub async fn succession_discharge_owed_sweep(
    nest: Arc<FfiNestClient>,
    successor_secret: Vec<u8>,
    accounts: Arc<FfiAccountRegistry>,
    old_store_path: Arc<dyn FfiSuccessorStorePath>,
    successor_store_path: Arc<dyn FfiSuccessorStorePath>,
) -> FfiOwedSweepAnswer {
    let answer = retry_group_sweep(
        nest,
        successor_secret,
        accounts,
        old_store_path,
        successor_store_path,
    )
    .await;
    let parked = answer.clone().into_owed_status();
    FfiOwedSweepAnswer {
        answer: answer.into(),
        parked: FfiSweepView::from(&parked),
        parked_state_json: parked.state_json().to_string(),
    }
}

/// The one body behind both sweep-retry exports.
async fn retry_group_sweep(
    nest: Arc<FfiNestClient>,
    successor_secret: Vec<u8>,
    accounts: Arc<FfiAccountRegistry>,
    old_store_path: Arc<dyn FfiSuccessorStorePath>,
    successor_store_path: Arc<dyn FfiSuccessorStorePath>,
) -> fauna_client_recovery::ceremony::SweepRetryAnswer {
    use fauna_client_recovery::ceremony::{
        SweepRetryAnswer, retry_predecessor, retry_sweep_as_successor,
    };

    let secret = match secret32(&successor_secret) {
        Ok(secret) => secret,
        Err(e) => return SweepRetryAnswer::Failed(format!("{e}")),
    };
    let successor_secret_hex = hex::encode(secret);
    let successor = ActorKeypair::from_secret(secret);
    // No succession into this identity recorded here at all: there is nothing
    // to finish, and the chain answer says so. Reachable only by a caller
    // driving the id directly — the button's render gate is a sweep report,
    // which only a succession produces — and answered rather than dropped
    // (testing.md point 11).
    let Some((old_actor_hex, old_secret_hex)) =
        retry_predecessor(accounts.registry(), &successor.actor_id_hex())
    else {
        return SweepRetryAnswer::NotLanded;
    };

    let answer = retry_sweep_as_successor(
        nest.nest_arc(),
        &successor_secret_hex,
        old_secret_hex.as_ref().map(|s| s.as_str()),
        move |old_actor_hex| PathBuf::from(old_store_path.mls_db_path(old_actor_hex.to_string())),
        move |successor_actor_hex| {
            PathBuf::from(successor_store_path.mls_db_path(successor_actor_hex.to_string()))
        },
        nest.conversations_engine().as_deref(),
    )
    .await;
    // The retry's roster joins the registry's parked ceremony (union by
    // person) and the post-store-ready pass drains it now — the one path for
    // the ceremony's roster and a retry's.
    #[cfg(feature = "recovery-aftermath")]
    if let SweepRetryAnswer::Swept(report) = &answer
        && let Ok(old) = fauna_core::hex32::decode(&old_actor_hex)
    {
        fauna_client_recovery::aftermath::PendingCeremony::repark_retried_roster(
            accounts.registry(),
            &successor.actor_id_hex(),
            &fauna_core::identity::ActorId(old),
            &report.unattested_members(),
        );
        crate::succession_aftermath::spawn_ledger_pass(nest.nest_arc(), &nest.ledger_pass_seams());
    }
    #[cfg(not(feature = "recovery-aftermath"))]
    let _ = old_actor_hex;
    answer
}

/// A succession that landed — the ceremony's whole outcome, in one record.
#[derive(uniffi::Record, Clone)]
pub struct FfiLandedSuccession {
    /// The successor identity's 64-hex secret.
    ///
    /// ⚠ **Already persisted by the time you read this** (step 1 of the module
    /// doc). It is returned anyway because of `persisted`: when the read-back
    /// says the device did not keep it, this value is the only copy in
    /// existence and must go **on screen** — see `persisted`.
    pub successor_secret_hex: String,
    /// The identity the account now belongs to, 64-char lowercase hex.
    pub new_actor_id_hex: String,
    /// Whether the successor seed was **verified present in the account store
    /// by a read-back** — never `add_account`'s return, which is infallible by
    /// signature and so proves only that the call was made.
    ///
    /// ⚠ `false` is the one arm where the secret must go on screen
    /// (`settings.recovery_kit.stolen_persist_failed`), and where the app must
    /// **NOT** tear its session down afterwards: doing so takes the only copy of
    /// the key with it. It is never phrased as "nothing happened" — the account
    /// *did* move.
    pub persisted: bool,
    /// What the pre-switch group sweep managed.
    pub sweep: FfiSweepView,
    /// The people the sweep can vouch for nothing about — raw 32-byte actor ids,
    /// the exhaustive convention — feeding the aftermath's member-review raise.
    ///
    /// **Empty is meaningful rather than absent, on every arm**: a sweep that
    /// found nobody, an engine that was never up, and a sweep that could not
    /// start all report "no one to review". This is the sweep report's own
    /// roster, never a re-derivation from the successor's engine (which would
    /// flag people who joined afterwards and miss people who have since left).
    pub review_roster: Vec<Vec<u8>>,
    /// The sweep in the e2e state provider's machine vocabulary, as a JSON
    /// string — `SweepStatus::state_json`, so an app's `/state` serializer
    /// republishes it rather than re-encoding the enum itself.
    ///
    /// ⚠ Deliberately a different vocabulary from [`FfiSweepView::kind`]
    /// (`no_engine` vs `no-engine`): one is for a journey, the other for a
    /// human. Do not paint from this.
    pub sweep_state_json: String,
    /// Unix seconds the nest applied the succession, when the submit reply
    /// carried it.
    ///
    /// **`None` on the reconcile arm**, where that reply never arrived. Not a
    /// dead end: the stamp is served by an authenticated kind, and the
    /// aftermath's email-filter raise asks for it on the successor's signed-in
    /// connection whenever it is handed `None`. Never substitute the statement's
    /// own clock — that is the *client's*, stamped at authoring, so a rule the
    /// thief added before the commit would escape the mark.
    pub succeeded_at: Option<i64>,
}

/// ⚠ Hand-written and redacted, for [`LandedSuccession`]'s reason: this holds an
/// identity seed, which *is* the account, and FFI records get traced.
impl std::fmt::Debug for FfiLandedSuccession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiLandedSuccession")
            .field("successor_secret_hex", &"<redacted>")
            .field("new_actor_id_hex", &self.new_actor_id_hex)
            .field("persisted", &self.persisted)
            .field("sweep", &self.sweep)
            .field("review_roster", &self.review_roster.len())
            .field("succeeded_at", &self.succeeded_at)
            .finish()
    }
}

impl From<&SweepStatus> for FfiSweepView {
    fn from(sweep: &SweepStatus) -> Self {
        let view = sweep.render_view();
        FfiSweepView {
            owes_work: view.owes_work(),
            kind: view.kind,
            detail: view.detail,
            groups: view.groups,
            groups_old_leaf_removed: view.groups_old_leaf_removed,
            unattested_members: view.unattested_members,
        }
    }
}

/// Build the FFI record from the shared outcome — every field a projection the
/// shared crate already computes, so nothing here decides anything.
fn landed_view(landed: &LandedSuccession, persisted: bool) -> FfiLandedSuccession {
    FfiLandedSuccession {
        successor_secret_hex: landed.successor_secret_hex.to_string(),
        new_actor_id_hex: landed.new_actor_id.to_hex(),
        persisted,
        sweep: FfiSweepView::from(&landed.sweep),
        review_roster: landed
            .sweep
            .review_roster()
            .iter()
            .map(|a| a.0.to_vec())
            .collect(),
        sweep_state_json: landed.sweep.state_json().to_string(),
        succeeded_at: landed.succeeded_at,
    }
}

/// How `identity-stolen-button`'s ceremony ended — the shared
/// [`StolenOutcome`], as a record an app paints without deciding anything
/// (`identity-succession.md` § Implementation status today, the *typed
/// outcome* ruling; the [`FfiSweepRetryAnswer`] shape).
///
/// Paint [`Self::message`] verbatim on `error-message` and wrap nothing: the
/// sentence carries its own headline, and only the `"not-landed"` one reads as a
/// failure (`settings.md` § Recovery kit → *The ceremony's outcome is headlined
/// by its arm*).
#[derive(uniffi::Record, Clone)]
pub struct FfiStolenOutcome {
    /// `"landed"` / `"not-landed"` / `"landed-for-another"` / `"undecided"` — a
    /// stable token, not a message. Only `"not-landed"` is a failure.
    pub kind: String,
    /// The sentence to render, as a `LocalizedText` the app resolves through its
    /// own pipeline. `None` on `"landed"` alone, whose outcome is the switch (or
    /// `stolen_persist_failed`, decided by [`FfiLandedSuccession::persisted`]).
    pub message: Option<fauna_core::localized::LocalizedText>,
    /// On `"landed"`, the succession. `None` on every other arm.
    pub landed: Option<FfiLandedSuccession>,
    /// Whether [`Self::message`] carries the only copy of the successor seed —
    /// the `"undecided"` arm whose persist was not verified. ⚠ Park it exactly
    /// as the persist-failure message is parked (so nothing else on the page
    /// clobbers it), and do NOT tear the session down. Carried as a flag so no
    /// app tells the two undecided halves apart by reading the key.
    pub carries_the_only_seed: bool,
}

/// ⚠ Hand-written and redacted: the undecided-unsaved message and the landed
/// record both hold an identity seed, and FFI records get traced.
impl std::fmt::Debug for FfiStolenOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiStolenOutcome")
            .field("kind", &self.kind)
            .field("message", &self.message.as_ref().map(|m| &m.key))
            .field("landed", &self.landed)
            .field("carries_the_only_seed", &self.carries_the_only_seed)
            .finish()
    }
}

/// Build the FFI record from the shared outcome. `persisted` is the read-back
/// verdict the landed arm carries; every other field is a shared projection.
fn stolen_outcome_view(outcome: &StolenOutcome, persisted: bool) -> FfiStolenOutcome {
    FfiStolenOutcome {
        kind: outcome.kind().to_string(),
        message: outcome.message(),
        landed: match outcome {
            StolenOutcome::Landed(landed) => Some(landed_view(landed, persisted)),
            _ => None,
        },
        carries_the_only_seed: outcome.carries_the_only_seed(),
    }
}

/// Steps 1–2 of [`succession_succeed_with_held_kit`]: persist the successor
/// seed into the account store and report whether it READS BACK — the verdict
/// that decides whether an app tells the user the seed is saved or puts it on
/// screen (`identity-succession.md` § Implementation status today). Lifted out
/// of the driver so a native test can pin the read-back over a store whose
/// writes vanish; takes the attempt's exposed parts, not the attempt, which this
/// crate cannot construct.
fn persist_successor_seed(
    registry: &fauna_client_accounts::AccountRegistry,
    successor_secret_hex: &str,
    successor_actor_hex: &str,
    nest_url: &str,
) -> bool {
    // ── Step 1: persist. A device id of its OWN, minted here — never the
    // predecessor's, and never left empty. `succession-aftermath.md` § Propagation → *Own device fleet*:
    // "device registrations and sync-agent renewal grants are **re-created**
    // under the new identity, not migrated", so carrying the old one across
    // would be the wrong answer even though the hardware did not change.
    //
    // ⚠ Left empty it is not merely untidy: an app that builds its session from
    // the account's stored material — apple reads `(secret, nest_url, device_id)`
    // and refuses to construct a client without all three — routes a perfectly
    // good successor into the onboarding wizard on the very next launch, i.e.
    // the ceremony appears to have signed the user out of the account it just
    // gave them back. Minted inside the boundary so every FFI app inherits it.
    let successor_device_id = hex::encode(fauna_client_core::chunking::generate_device_id());
    if let Err(e) = registry.add_account(
        successor_secret_hex,
        Some(nest_url),
        Some(&successor_device_id),
    ) {
        // Logged, never fatal: the account HAS moved by now, and returning an
        // error here would drop the only copy of the seed that owns it.
        tracing::error!(error = %e, "succession: persisting the successor seed failed");
    }

    // ── Step 2: verify by READ-BACK, never off `add_account`'s return.
    let persisted = registry
        .secrets(successor_actor_hex)
        .is_some_and(|stored| stored.secret_hex.as_str() == successor_secret_hex);
    if !persisted {
        tracing::error!("succession: the successor seed did not read back from the account store");
    }
    persisted
}

/// Run the whole "my identity was stolen" ceremony with the kit the user holds
/// — `identity-stolen-button`'s one call
/// (`docs/goal/ui/settings.md` § Recovery kit).
///
/// The account is re-pointed to a freshly minted successor, the successor's seed
/// is persisted and verified here, the old identity's MLS groups are swept, and
/// the predecessor → successor link is recorded. **Irreversible**: the old
/// identity stops working the moment the nest commits, and the caller must have
/// gated this behind `identity-stolen-confirm-field` reading the literal
/// `SUCCEED` (never localized) before reaching it.
///
/// ## What the caller still owes, after this returns Ok
///
/// 1. **Activate the successor and re-launch the session.** This function does
///    not switch accounts — the nest revokes the old identity's bearers inside
///    the succession transaction, so the caller's live session is already dead,
///    and tearing it down is the app's own lifecycle. ⚠ **Unless `persisted` is
///    false**, where tearing down takes the only copy of the seed with it.
/// 2. **Show the successor a fresh kit.** The old kit retired with the old
///    identity and the nest deleted its escrow row inside the same transaction,
///    so until a new one is created there is no route back into this account but
///    the 30-day seed-alone window.
/// 3. **The aftermath** — the corpus re-seal, the capability-grant re-mint, the
///    member-review raise over `review_roster` — is its own pass, and this
///    ceremony deliberately stops before it. A surface must say so rather than
///    implying the account is fully restored.
///
/// ## Parameters
///
/// `nest_url` is the account's nest URL; the ceremony opens its **own**
/// connections from it (anonymous for the succession, then as the successor for
/// the sweep) rather than riding `nest`'s authenticated one — which is the whole
/// reason this works for an owner a thief has locked out. `old_secret` is the
/// current identity's 32-byte Ed25519 secret. `kit_input` is whatever the user
/// pasted into `recovery-entry-phrase-field`: the shared `parse_kit` grammar
/// takes both the `fauna://recovery` URI form and a bare 64-hex secret, and a
/// malformed one costs no round trip.
///
/// An `Err` here means the ceremony could not even start (unparseable secret
/// bytes) — nothing was minted. Every ceremony that ran, including every
/// refusal before the submit, returns `Ok` with its arm in
/// [`FfiStolenOutcome::kind`]; the arms that carry a seed say so through
/// `persisted` / `carries_the_only_seed`.
#[fauna_uniffi_async::export]
pub async fn succession_succeed_with_held_kit(
    nest: Arc<FfiNestClient>,
    nest_url: String,
    old_secret: Vec<u8>,
    kit_input: String,
    accounts: Arc<FfiAccountRegistry>,
    store_path: Arc<dyn FfiSuccessorStorePath>,
) -> Result<FfiStolenOutcome, FfiError> {
    let identity = ActorKeypair::from_secret(secret32(&old_secret)?);
    let old_actor_id = identity.actor_id();

    // The old identity's live engine, read off this client's own session rather
    // than passed in (module doc). `None` is an ordinary answer.
    let old_engine = nest.conversations_engine();

    // The resolver, wrapped once so both arms pass the same `FnOnce`. It is
    // invoked by the ceremony AFTER the successor's connect — never here.
    let db_path_for = move |successor_actor_hex: &str| -> PathBuf {
        PathBuf::from(store_path.mls_db_path(successor_actor_hex.to_string()))
    };

    let client = RecoveryClient::new(nest.nest_arc());
    // No status re-read first: the kit is the whole authorization, and a chain
    // read here would only add a round trip a locked-out owner can fail on.
    // A refusal before the submit is the not-landed arm, not a fault: nothing
    // moved and the seed it minted authorizes nothing.
    let attempt =
        match succeed_with_held_kit(&client, old_actor_id, &kit_input, Some(&identity)).await {
            Ok(attempt) => attempt,
            Err(e) => return Ok(stolen_outcome_view(&StolenOutcome::not_landed(e), false)),
        };

    // ── Steps 1–2: persist the seed, off the attempt, BEFORE matching arms, and
    // verify it by read-back (`persist_successor_seed`). Reachable without
    // matching on purpose — "persist before anything that can fail or block"
    // does not depend on which arm this is.
    let registry = accounts.registry();
    let successor_secret_hex = attempt.successor_secret_hex().to_string();
    let successor_actor_hex = attempt.successor_actor_id().to_hex();
    let persisted = persist_successor_seed(
        registry,
        &successor_secret_hex,
        &successor_actor_hex,
        &nest_url,
    );

    // ── Step 3: the arms. The confirmed one lands; only the unconfirmed one has
    // to ask the nest whether the account moved at all.
    let outcome = match attempt {
        SuccessionAttempt::Confirmed(handoff) => {
            let sweep =
                sweep_after_succession(&nest_url, old_engine.as_deref(), &handoff, db_path_for)
                    .await;
            StolenOutcome::Landed(LandedSuccession::new(
                handoff.successor_secret_hex().to_string(),
                handoff.new_actor_id,
                sweep,
                handoff.succeeded_at,
            ))
        }
        SuccessionAttempt::Unconfirmed(unconfirmed) => {
            finish_unconfirmed_succession(
                &nest_url,
                old_engine.as_deref(),
                unconfirmed.old_actor_id,
                unconfirmed.successor_secret_hex(),
                &unconfirmed.error,
                registry,
                db_path_for,
            )
            .await
        }
    };
    let StolenOutcome::Landed(landed) = &outcome else {
        return Ok(stolen_outcome_view(&outcome, persisted));
    };

    // ── Step 4: the LINK, after the arm resolved — never beside `add_account`.
    // Never fatal: every aftermath consumer degrades quietly without it, but a
    // succession that landed must not be reported as a failure over a bookkeeping
    // write (`FfiAccountRegistry::record_succession`).
    //
    // Parked FIRST, in the same registry: what only this ceremony knows (the
    // sweep's roster, the nest's commit stamp) is the member-item and
    // filter-mark raises' input, drained by the successor's post-store-ready
    // pass — the single durable decision point of those raises, so no app
    // carries it across its account switch
    // (`fauna_client_recovery::aftermath::PendingCeremony`).
    #[cfg(feature = "recovery-aftermath")]
    fauna_client_recovery::aftermath::PendingCeremony::new(
        &old_actor_id,
        &landed.sweep.review_roster(),
        landed.succeeded_at,
    )
    .park(registry, &landed.new_actor_id.to_hex());
    if let Err(e) =
        registry.record_succession(&old_actor_id.to_hex(), &landed.new_actor_id.to_hex())
    {
        tracing::error!(error = %e, "succession: recording the predecessor link failed");
    }

    Ok(stolen_outcome_view(&outcome, persisted))
}

// ─────────────────────────────────────────────────────────────────────────────
// The rest of the section: the status read and the three kit ceremonies.
// ─────────────────────────────────────────────────────────────────────────────

/// The Settings section's state, with **which actions it enables already
/// decided** (`ui/settings.md` § Recovery kit → *The four actions*).
///
/// ⚠ The booleans are the point. Every one is a shared predicate
/// (`RecoveryKitStatus::allows_*`), and they do **not** all follow from `kind`
/// in the way a renderer would guess: `allows_stolen` is unconditionally true —
/// theft is exactly the case where no kit was ever created — and `allows_replace`
/// stays true *during* a pending window. An app deriving enablement from `kind`
/// would get both wrong, which is why `kind` is here for the status LINE and
/// nothing else.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiRecoveryKitStatus {
    /// `"never-created"` / `"registered"` / `"registered-no-escrow"` /
    /// `"replacement-pending"` — a stable token for the status line's copy.
    ///
    /// ⚠ `"registered-no-escrow"` is a real loss-protection gap, not an error:
    /// the chain has a kit but no escrow blob rests, so recovery *by phrase* is
    /// unavailable until a re-put (succession still works). Its copy must say
    /// both halves plainly, and it is the only state that renders
    /// `recovery-kit-escrow-reseal-button`.
    pub kind: String,
    /// `recovery-kit-create-button` — never-created only.
    pub allows_create: bool,
    /// `recovery-kit-replace-button` — every registered state, **including a
    /// pending window**.
    pub allows_replace: bool,
    /// `recovery-kit-lost-button` — registered states only.
    pub allows_lost: bool,
    /// `identity-stolen-button` — **every** state, per § Recovery kit's
    /// "stolen (any)". Its only gate is the type-to-confirm field.
    pub allows_stolen: bool,
    /// `recovery-kit-escrow-reseal-button` — the no-escrow state only.
    pub allows_escrow_reseal: bool,
    /// The pending replacement's new public half, 64-hex — rendered so the user
    /// can compare it against a kit they hold: if it is not theirs, the request
    /// was not theirs either. `None` outside a pending window.
    pub pending_new_pubkey_hex: Option<String>,
    /// Unix seconds the pending replacement lands if uncontested. `None` outside
    /// a pending window; pair it with `recovery-pending-veto-button`.
    pub pending_lands_at: Option<i64>,
}

impl From<&RecoveryKitStatus> for FfiRecoveryKitStatus {
    fn from(status: &RecoveryKitStatus) -> Self {
        let pending = match status {
            RecoveryKitStatus::ReplacementPending(p) => Some(p),
            _ => None,
        };
        FfiRecoveryKitStatus {
            kind: match status {
                RecoveryKitStatus::NeverCreated => "never-created",
                RecoveryKitStatus::Registered => "registered",
                RecoveryKitStatus::RegisteredNoEscrow => "registered-no-escrow",
                RecoveryKitStatus::ReplacementPending(_) => "replacement-pending",
            }
            .to_string(),
            allows_create: status.allows_create(),
            allows_replace: status.allows_replace(),
            allows_lost: status.allows_lost(),
            allows_stolen: status.allows_stolen(),
            allows_escrow_reseal: status.allows_escrow_reseal(),
            pending_new_pubkey_hex: pending.map(|p| p.new_recovery_pubkey_hex.clone()),
            pending_lands_at: pending.map(|p| p.lands_at),
        }
    }
}

/// A kit a ceremony just minted — shown **once**, never persisted.
///
/// ⚠ There is no path that shows it again and there can never be one: the
/// secret is offline-only (`identity-succession.md` § The RecoveryKey —
/// *Custody*), so nothing on the device holds it after the screen closes. A
/// surface that drops this without displaying it leaves a kit nobody holds.
#[derive(uniffi::Record, Clone)]
pub struct FfiMintedKit {
    /// The 64-hex recovery secret. Render it into
    /// `recovery-kit-secret-display` / `-qr`; do not log, cache or store it.
    pub secret_hex: String,
    /// Whether the seed-escrow blob landed alongside the registration.
    ///
    /// ⚠ `false` must **NOT** be rendered as a plain error. The registration has
    /// already landed by then, so the returned secret is the only copy in
    /// existence — showing an error instead of the kit destroys the account's
    /// only route back. Say the kit is live *and* that phrase-recovery is not
    /// yet armed.
    pub escrow_stored: bool,
    /// Unix seconds the seed-alone replacement lands, for the `lost` ceremony
    /// only — that one opens a 30-day window rather than taking effect now.
    /// `None` for create and replace, which land immediately.
    pub lands_at: Option<i64>,
}

/// ⚠ Redacted, like [`FfiLandedSuccession`]'s: this holds a recovery root.
impl std::fmt::Debug for FfiMintedKit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiMintedKit")
            .field("secret_hex", &"<redacted>")
            .field("escrow_stored", &self.escrow_stored)
            .field("lands_at", &self.lands_at)
            .finish()
    }
}

/// Resolve the predecessor seeds an escrow-writing ceremony must carry, from
/// this device's account registry.
///
/// ⚠ **Never call `create_kit`/`reseal_escrow_with_held_kit` with an empty slice
/// you did not get from here.** `predecessors` is a parameter rather than an
/// internal default precisely so the site that must be non-empty cannot be
/// forgotten silently, and a blob written without seeds it should have carried
/// re-opens the device-loss race the escrow backstop closes — with no error at
/// any layer. The decode itself is shared with tui and linux
/// (`fauna_client_recovery::predecessor_seeds_from_rows`).
fn predecessors_for(
    registry: &fauna_client_accounts::AccountRegistry,
    identity: &ActorKeypair,
) -> Vec<fauna_client_recovery::PredecessorSeed> {
    predecessor_seeds_from_rows(registry.predecessor_seeds(&identity.actor_id_hex()))
}

/// The **verified** successor of a refused identity — the launch `superseded`
/// screen's upgrade from the nest's *claim* to a proven fact
/// (`identity-succession.md` § Propagation → *Own device fleet*). Resolves the
/// successor's 64-hex actor id, or `None` when the registration chain
/// authorizes none. The FFI twin of wasm's `resolveVerifiedSuccessor` and
/// linux's `verify_succession_successor`.
///
/// **Anonymous by necessity, not convenience:** the refused identity cannot
/// authenticate — that is what the refusal means — so this opens its own
/// pre-identity connection to `nest_url` and rides `succession.lookup` /
/// `registration.chain`, both pre-identity kinds.
///
/// **The successor the refusal NAMED is deliberately not a parameter.**
/// `resolve_successor` returns what the chain *authorizes*; the nest is
/// enforcer and distributor, never authorizer, so a claim that disagrees with
/// the chain is a lie to log, never something to render.
///
/// Errors **only** on a malformed secret. Every expected failure — unreachable
/// nest, empty lookup, a non-contiguous or hostile chain — is `None`, because
/// the caller's correct fallback is the claim-free message it already shows.
#[fauna_uniffi_async::export]
pub async fn succession_resolve_verified_successor(
    nest_url: String,
    old_secret: Vec<u8>,
) -> Result<Option<String>, FfiError> {
    let old_actor_id = ActorKeypair::from_secret(secret32(&old_secret)?).actor_id();
    let anon = match fauna_anon_client::AnonymousNestClient::connect(&nest_url).await {
        Ok(anon) => anon,
        Err(e) => {
            tracing::warn!("[launch] could not reach the nest to verify the succession: {e}");
            return Ok(None);
        }
    };
    let client = RecoveryClient::new(anon);
    match fauna_client_recovery::resolve_successor(&client, old_actor_id, None).await {
        Ok(Some(verified)) => Ok(Some(verified.new_actor_id.to_hex())),
        Ok(None) => {
            // Refused as superseded, yet the chain shows no succession. Nothing
            // to tell the user — but exactly the disagreement an admin wants.
            tracing::warn!("[launch] refused as superseded, yet the chain shows no succession");
            Ok(None)
        }
        Err(e) => {
            tracing::warn!("[launch] could not verify the succession: {e}");
            Ok(None)
        }
    }
}

/// Read the section's state — `recovery-kit-status` plus every action's
/// enablement, in one round trip.
///
/// Read from the **registration chain**, never a local flag, so a kit created on
/// another device is reflected here.
#[fauna_uniffi_async::export]
pub async fn recovery_kit_status(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
) -> Result<FfiRecoveryKitStatus, FfiError> {
    let identity = ActorKeypair::from_secret(secret32(&secret)?);
    let client = RecoveryClient::new(nest.nest_arc());
    let status = kit_status(&client, &identity.actor_id())
        .await
        .map_err(general_err)?;
    Ok(FfiRecoveryKitStatus::from(&status))
}

/// Whole days left in a pending replacement window, rounded **up** so a
/// window in its final hour still reads N days, not 0 — the same rule
/// `RecoveryKitStatus::status_line` applies for the two apps (linux, tui)
/// that call it directly (`fauna_client_recovery::status`); the FFI apps had
/// no door to it, and apple was reimplementing the rounding by hand
/// (`RecoveryKitSection.swift`'s `pendingDays`).
///
/// Pure — pass a fresh `now` (unix seconds) each render for a live-updating
/// countdown with no re-fetch, mirroring `pending_lands_at`'s own contract:
/// `lands_at` is [`FfiRecoveryKitStatus::pending_lands_at`] unwrapped.
#[uniffi::export]
pub fn recovery_pending_days_remaining(lands_at: i64, now: i64) -> u64 {
    fauna_client_recovery::replacement::days_remaining_from(lands_at, now)
}

/// The `fauna://recovery` URI behind a minted kit's QR **and** copy button —
/// never the bare [`FfiMintedKit::secret_hex`], which is only the on-screen
/// display (`identity-succession.md` § The RecoveryKey, *Which encoding each
/// affordance carries*: a copied kit must restore knowing its account, exactly
/// as a scanned one does). The FFI door to the builder tui, linux and web call
/// (`fauna_client_recovery::kit_display_uri`), so no app re-derives it.
///
/// `secret` is the account's identity seed (the same argument the ceremonies
/// take — the actor id is derived from it); `handle` may be the bare local part
/// or `user@host` and is qualified with `node_url`'s host; empty means none.
/// Pure — no round trip.
#[uniffi::export]
pub fn recovery_kit_display_uri(
    kit_secret_hex: String,
    secret: Vec<u8>,
    handle: String,
    node_url: String,
) -> Result<String, FfiError> {
    let identity = ActorKeypair::from_secret(secret32(&secret)?);
    Ok(fauna_client_recovery::kit_display_uri(
        &kit_secret_hex,
        &identity.actor_id_hex(),
        &handle,
        &node_url,
    ))
}

/// `recovery-kit-create-button` and `recovery-kit-replace-button` — ONE export,
/// because they are one ceremony with two authorization arms.
///
/// `held_kit_input` is what the user pasted into `recovery-entry-phrase-field`
/// (the `fauna://recovery` URI form or a bare 64-hex secret, both taken by the
/// shared `parse_kit` grammar): `None` is the **create** arm (first
/// registration), `Some` the **replace** arm authorized by the prior key. Which
/// arm is legal follows from the status the section already read — there is no
/// second predicate here.
///
/// The escrow blob is re-put in the SAME ceremony, which the seed-escrow
/// lifecycle requires: the nest deletes the escrow row the moment a registration
/// changes the pubkey, so a replace that did not re-put would leave the account
/// with no phrase recovery at all.
///
/// ⚠ On the replace arm this reads the resting blob **before** it registers and
/// writes the union of its predecessor section with this device's own, refusing
/// rather than narrowing if that read fails — so a device that never held a
/// predecessor's row cannot destroy a section it cannot see. That is the shared
/// crate's behaviour, inherited, not re-implemented.
#[fauna_uniffi_async::export]
pub async fn recovery_create_kit(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
    accounts: Arc<FfiAccountRegistry>,
    held_kit_input: Option<String>,
) -> Result<FfiMintedKit, FfiError> {
    let identity = ActorKeypair::from_secret(secret32(&secret)?);
    let client = RecoveryClient::new(nest.nest_arc());
    let predecessors = predecessors_for(accounts.registry(), &identity);

    // Parse first: a malformed phrase must cost no round trip.
    let prior = match held_kit_input.as_deref() {
        Some(input) => Some(parse_kit(input).map_err(general_err)?),
        None => None,
    };
    let kit = create_kit(
        &client,
        &identity,
        prior.as_ref().map(|p| &p.recovery),
        &predecessors,
    )
    .await
    .map_err(general_err)?;

    // The chain moved: carry it to every linked nest now, not at the next
    // full pass (`identity-succession.md` § Enforcement on the home nest →
    // *Every nest the identity is linked to*). Shared here so all FFI apps
    // inherit it; fire-and-forget, and a runtime not up yet leaves it to the
    // next pass.
    if let Some(store) = crate::account_runtime::handle() {
        store.registration_chain_moved();
    }

    // The chain moved, so mirror the head onto the profile — the one step the
    // kit ceremony leaves to its caller, and for an FFI app THIS is the caller.
    // Done here rather than in each app's own language so macOS, iOS, windows
    // and android inherit it (priority #2) instead of each re-deriving the
    // home-nest entry and the best-effort arm, which is exactly how web shipped
    // this call's sibling gap. Never fatal: the secret is already on its way to
    // the screen, and the registration chain — not this cache — is authoritative.
    let profile_predecessors = fauna_client_profile::predecessors_from_hex(
        &accounts
            .registry()
            .predecessors_of(&identity.actor_id_hex()),
    );
    fauna_client_recovery::ceremony::mirror_recovery_head(
        nest.nest_arc(),
        &identity,
        &profile_predecessors,
        &kit,
    )
    .await;

    Ok(FfiMintedKit {
        secret_hex: kit.secret_hex().to_string(),
        escrow_stored: kit.escrow.is_stored(),
        lands_at: None,
    })
}

/// Register the kit the onboarding `recovery_kit` screen minted and the user
/// confirmed, at the wizard's signed-in handoff — `kit_hex` is what
/// `OnboardingMachine::take_pending_recovery_secret` handed over. The FFI face
/// of `fauna_client_recovery::ceremony::register_deferred_kit` (tui and linux
/// call it in-process; web's twin is wasm `recoveryRegisterDeferredKit`): a
/// first registration of THAT root (never a freshly minted one — the user has
/// just written this one down), the profile-head mirror, every arm logged.
/// Resolves once the attempt settles and never errors on a ceremony failure:
/// Settings' `recovery-kit-status` tells the truth (never-created, or
/// registered-no-escrow). Errors only on a malformed identity secret.
#[fauna_uniffi_async::export]
pub async fn recovery_register_deferred_kit(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
    kit_hex: String,
) -> Result<(), FfiError> {
    let identity = ActorKeypair::from_secret(secret32(&secret)?);
    let kit_hex = fauna_core::secret::SecretString::from(kit_hex);
    fauna_client_recovery::ceremony::register_deferred_kit(nest.nest_arc(), &identity, &kit_hex)
        .await;
    Ok(())
}

/// `recovery-kit-lost-button` — the seed-alone replacement, which **opens the
/// 30-day window rather than taking effect now**, at the bound nest and every
/// linked nest.
///
/// The distinction is the whole feature: a thief holding the seed can start this
/// too, which is why it waits and why every device gets a loud alarm plus a
/// standing banner carrying `recovery-pending-veto-button`. Like create, it
/// mints a kit and shows its secret once — the window governs when it *lands*,
/// not when it is generated, so `lands_at` is set and the secret must still be
/// displayed immediately.
#[fauna_uniffi_async::export]
pub async fn recovery_request_seed_alone_replacement(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
) -> Result<FfiMintedKit, FfiError> {
    let identity = ActorKeypair::from_secret(secret32(&secret)?);
    let client = RecoveryClient::new(nest.nest_arc());
    // At the bound nest, then at every linked nest (`identity-succession.md`
    // § Enforcement on the home nest, clause (c)); a linked nest it cannot
    // reach is logged and owed by the runtime's secondary leg.
    let (pending, _linked) = request_seed_alone_replacement_everywhere(
        &client,
        &identity,
        &NativeLinkedNestDial::new(&identity),
    )
    .await
    .map_err(general_err)?;
    Ok(FfiMintedKit {
        secret_hex: pending.secret_hex().to_string(),
        // No blob is written on this arm: the window governs whether the
        // registration ever lands, and the nest deletes the escrow row itself
        // when it does.
        escrow_stored: false,
        lands_at: Some(pending.lands_at),
    })
}

/// `recovery-pending-veto-button` — contest the pending replacement with the kit
/// you hold, at the bound nest and every linked nest. Returns whether something
/// was actually pending to cancel at any of them.
///
/// Pre-identity and challenge-gated by the shared ceremony: a bare replayable
/// signed veto would let a captured one cancel any future honest replacement
/// forever, so each veto spends a single-use nonce and contests whatever
/// currently pends.
#[fauna_uniffi_async::export]
pub async fn recovery_veto_pending_replacement(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
    held_kit_input: String,
) -> Result<bool, FfiError> {
    let identity = ActorKeypair::from_secret(secret32(&secret)?);
    let client = RecoveryClient::new(nest.nest_arc());
    let kit = parse_kit(&held_kit_input).map_err(general_err)?;
    // At the bound nest and every linked nest, each over its own challenge
    // (clause (c)); the bound nest's refusal is the gesture's error, a linked
    // nest that could not be asked is logged.
    let (bound, linked) = veto_everywhere(
        &client,
        &kit,
        identity.actor_id(),
        &NativeLinkedNestDial::new(&identity),
    )
    .await;
    Ok(bound.map_err(general_err)?
        || linked
            .nests
            .iter()
            .any(|n| matches!(n.outcome, LinkedNestOutcome::Answered(true))))
}

/// `recovery-kit-escrow-reseal-button` — the no-escrow repair: re-put the escrow
/// blob with the kit already in hand, **without retiring that kit**.
///
/// Renders only in the `"registered-no-escrow"` state. It is deliberately not
/// `create_kit`: the user's kit is fine, only the blob is missing, and minting a
/// new one would retire a key they are still holding. Returns the unix seconds
/// the blob was stored at.
#[fauna_uniffi_async::export]
pub async fn recovery_reseal_escrow_with_held_kit(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
    accounts: Arc<FfiAccountRegistry>,
    held_kit_input: String,
) -> Result<i64, FfiError> {
    let identity = ActorKeypair::from_secret(secret32(&secret)?);
    let client = RecoveryClient::new(nest.nest_arc());
    let kit = parse_kit(&held_kit_input).map_err(general_err)?;
    let predecessors = predecessors_for(accounts.registry(), &identity);
    reseal_escrow_with_held_kit(&client, &identity, &kit, &predecessors)
        .await
        .map_err(general_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The render view is the SHARED projection, not a mapping written here —
    /// pin the three arms so a second answer cannot creep back in.
    #[test]
    fn sweep_view_reports_the_shared_arms() {
        assert_eq!(
            FfiSweepView::from(&SweepStatus::NoEngine),
            FfiSweepView {
                kind: "no-engine".to_string(),
                detail: None,
                groups: 0,
                groups_old_leaf_removed: 0,
                unattested_members: 0,
                owes_work: true,
            }
        );
        assert_eq!(
            FfiSweepView::from(&SweepStatus::Failed("engine would not build".to_string())),
            FfiSweepView {
                kind: "failed".to_string(),
                detail: Some("engine would not build".to_string()),
                groups: 0,
                groups_old_leaf_removed: 0,
                unattested_members: 0,
                owes_work: true,
            }
        );
    }

    /// The lines an app paints are the SHARED selection, reached through the
    /// view it carried across the switch — pinned at this boundary so an app
    /// cannot be handed the pieces to select them a second way. The
    /// `groups == 0` silence and the retry-affordance split are the shared
    /// crate's own tests; this checks only that the boundary delegates.
    #[test]
    fn sweep_copy_is_the_shared_selection_over_the_carried_view() {
        use fauna_client_recovery::ceremony::SweepRetryAffordance;
        let none = FfiSweepView::from(&SweepStatus::NoEngine);
        let without = sweep_copy(none.clone(), false);
        let with = sweep_copy(none, true);
        assert_eq!(
            without.outcome,
            SweepStatus::NoEngine
                .render_copy(SweepRetryAffordance::Absent)
                .outcome
        );
        assert_eq!(
            with.outcome,
            SweepStatus::NoEngine
                .render_copy(SweepRetryAffordance::Rendered)
                .outcome
        );
        assert_ne!(
            with.outcome, without.outcome,
            "the degraded arm names the button only where the app says it renders it"
        );

        let empty = FfiSweepView::from(&SweepStatus::Ran(Box::default()));
        assert!(!empty.owes_work, "nothing to move, nothing to finish");
        let copy = sweep_copy(empty, false);
        assert_eq!(
            (copy.outcome, copy.unattested),
            (None, None),
            "a sweep over no groups says nothing — a shared rule, not an app's"
        );
    }

    /// `no-engine` carries an EMPTY roster rather than no roster — the trap the shared `review_roster` spells out for every
    /// arm, re-pinned at this boundary because a `Vec` that arrives empty and a
    /// field that arrives absent look identical to an app.
    #[test]
    fn a_sweep_that_never_ran_reports_an_empty_roster_not_an_absent_one() {
        let landed = LandedSuccession::new(
            "aa".repeat(32),
            fauna_core::identity::ActorId([7u8; 32]),
            SweepStatus::NoEngine,
            None,
        );
        let view = landed_view(&landed, true);
        assert!(view.review_roster.is_empty());
        assert_eq!(view.sweep.kind, "no-engine");
        assert_eq!(view.new_actor_id_hex, "07".repeat(32));
    }

    /// The ceremony's record is the SHARED outcome: each arm crosses with its
    /// own token and sentence, the landed arm alone with a succession and no
    /// sentence, and the undecided-unsaved arm alone flagged for parking.
    #[test]
    fn the_stolen_outcome_crosses_with_the_shared_arms() {
        use fauna_client_recovery::ceremony::STOLEN_KIND_LANDED;
        let landed = stolen_outcome_view(
            &StolenOutcome::Landed(LandedSuccession::new(
                "aa".repeat(32),
                fauna_core::identity::ActorId([7u8; 32]),
                SweepStatus::NoEngine,
                None,
            )),
            true,
        );
        assert_eq!(landed.kind, STOLEN_KIND_LANDED);
        assert!(landed.message.is_none());
        assert!(landed.landed.as_ref().is_some_and(|l| l.persisted));
        assert!(!landed.carries_the_only_seed);

        let reported = fauna_client_recovery::RecoveryError::Transport("gone".to_string());
        for (outcome, parks) in [
            (StolenOutcome::not_landed("nest refused"), false),
            (
                StolenOutcome::LandedForAnother {
                    new_actor_id: fauna_core::identity::ActorId([9u8; 32]),
                },
                false,
            ),
            (
                StolenOutcome::undecided("cause".into(), true, &"bb".repeat(32), &reported),
                false,
            ),
            (
                StolenOutcome::undecided("cause".into(), false, &"bb".repeat(32), &reported),
                true,
            ),
        ] {
            let crossed = stolen_outcome_view(&outcome, false);
            assert_eq!(crossed.kind, outcome.kind());
            assert_eq!(crossed.message, outcome.message(), "{}", crossed.kind);
            assert!(crossed.message.is_some() && crossed.landed.is_none());
            assert_eq!(crossed.carries_the_only_seed, parks, "{}", crossed.kind);
        }
    }

    /// The two vocabularies stay apart: the human-facing arm token is
    /// hyphenated, the journey's is underscored, and an app that painted from
    /// `sweep_state_json` would be reading the wrong one.
    #[test]
    fn the_state_json_vocabulary_is_not_the_render_vocabulary() {
        let landed = LandedSuccession::new(
            "bb".repeat(32),
            fauna_core::identity::ActorId([1u8; 32]),
            SweepStatus::NoEngine,
            Some(1_724_000_000),
        );
        let view = landed_view(&landed, false);
        assert_eq!(view.sweep.kind, "no-engine");
        assert!(view.sweep_state_json.contains("no_engine"));
        assert_eq!(view.succeeded_at, Some(1_724_000_000));
    }

    /// Every retry answer that swept nothing crosses with a sentence and no
    /// view; the one that swept crosses with a view and no sentence.
    ///
    /// The split is the whole contract of the record: an app paints `message`
    /// on `error-message` and, on the swept arm alone, replaces the carried
    /// `FfiSweepView` — so an arm that carried both would let a surface report
    /// the same press twice, and one that carried neither would be the dropped
    /// command testing.md point 11 forbids.
    #[test]
    fn a_retry_answers_either_in_words_or_with_a_fresh_view_never_both() {
        use fauna_client_recovery::ceremony::SweepRetryAnswer;

        for answer in [
            SweepRetryAnswer::NoOldState,
            SweepRetryAnswer::NotLanded,
            SweepRetryAnswer::LandedForAnother,
            SweepRetryAnswer::Failed("rpc disconnected".to_string()),
        ] {
            let kind = answer.kind().to_string();
            let crossed = FfiSweepRetryAnswer::from(answer);
            assert_eq!(crossed.kind, kind);
            assert!(
                crossed.message.is_some(),
                "{kind} must answer in words — the button is on screen on every device"
            );
            assert!(crossed.sweep.is_none(), "{kind} swept nothing");
            assert!(crossed.sweep_state_json.is_none(), "{kind} swept nothing");
            assert!(crossed.review_roster.is_empty(), "{kind} swept nobody");
        }

        let swept = FfiSweepRetryAnswer::from(SweepRetryAnswer::Swept(Box::default()));
        assert_eq!(swept.kind, "swept");
        assert!(
            swept.message.is_none(),
            "a press that swept renders through the sweep's own lines, not a second sentence"
        );
        let view = swept
            .sweep
            .expect("the fresh view replaces the carried one");
        assert_eq!(
            view.kind, "ran",
            "the arm is the SHARED projection's, not ours"
        );
        assert!(
            swept
                .sweep_state_json
                .is_some_and(|json| json.contains("\"status\"")),
            "the journey's witness rides along, in the state protocol's own shape"
        );
    }

    /// **The two enablement traps, pinned.** `allows_stolen` is true even with
    /// NO kit ever created (theft is exactly that case), and `allows_replace`
    /// stays true *during* a pending window. A renderer deriving enablement from
    /// `kind` gets both wrong — which is why the booleans cross the boundary.
    #[test]
    fn enablement_does_not_follow_from_the_status_kind() {
        let never = FfiRecoveryKitStatus::from(&RecoveryKitStatus::NeverCreated);
        assert_eq!(never.kind, "never-created");
        assert!(never.allows_create);
        assert!(
            never.allows_stolen,
            "stolen is enabled in EVERY state — a thief who took the seed before \
             a kit existed is the whole reason"
        );
        assert!(!never.allows_replace);
        assert!(!never.allows_lost);
        assert!(!never.allows_escrow_reseal);

        let pending = FfiRecoveryKitStatus::from(&RecoveryKitStatus::ReplacementPending(
            fauna_client_recovery::PendingReplacement {
                new_recovery_pubkey_hex: "ab".repeat(32),
                requested_at: 1_724_000_000,
                lands_at: 1_726_592_000,
            },
        ));
        assert_eq!(pending.kind, "replacement-pending");
        assert!(
            pending.allows_replace,
            "replace stays available DURING the window — it is how an owner who \
             still holds their kit ends the window immediately"
        );
        assert_eq!(pending.pending_new_pubkey_hex, Some("ab".repeat(32)));
        assert_eq!(pending.pending_lands_at, Some(1_726_592_000));
    }

    /// The FFI door mirrors the shared crate's own rounding pin
    /// (`fauna_client_recovery::status::tests::pending_countdown_rounds_up`) —
    /// this is the function apple actually calls, so it gets its own pin
    /// rather than trusting the shared test alone.
    #[test]
    fn pending_days_remaining_rounds_up_like_the_shared_rule() {
        let day = 86_400i64;
        assert_eq!(recovery_pending_days_remaining(30 * day, 0), 30);
        assert_eq!(
            recovery_pending_days_remaining(2 * day - 3600, 0),
            2,
            "47 hours left is not yet 1 day"
        );
        assert_eq!(
            recovery_pending_days_remaining(3600, 0),
            1,
            "the final hour still reads a day, never zero"
        );
        assert_eq!(
            recovery_pending_days_remaining(0, day),
            0,
            "already elapsed saturates rather than wrapping"
        );
    }

    /// The no-escrow state is the only one that offers the repair, and it is a
    /// gap rather than an error — `allows_replace` is still true there, so a
    /// surface must not present the reseal as the only way out.
    #[test]
    fn only_the_no_escrow_state_offers_the_reseal_repair() {
        let registered = FfiRecoveryKitStatus::from(&RecoveryKitStatus::Registered);
        assert!(!registered.allows_escrow_reseal);
        assert_eq!(registered.pending_new_pubkey_hex, None);

        let gap = FfiRecoveryKitStatus::from(&RecoveryKitStatus::RegisteredNoEscrow);
        assert_eq!(gap.kind, "registered-no-escrow");
        assert!(gap.allows_escrow_reseal);
        assert!(gap.allows_replace);
        assert!(gap.allows_stolen);
    }

    /// The FFI apps' copy button and QR get the same account-naming URI the
    /// Rust apps build — never the bare hex a copied kit used to carry on
    /// windows and apple.
    #[test]
    fn the_display_uri_door_names_the_account() {
        let seed = [7u8; 32];
        let actor = ActorKeypair::from_secret(seed).actor_id_hex();
        let kit = "ab".repeat(32);
        let uri = recovery_kit_display_uri(
            kit.clone(),
            seed.to_vec(),
            "ada".into(),
            "https://nest.example".into(),
        )
        .expect("a 32-byte seed");
        assert_eq!(
            uri,
            format!("fauna://recovery?secret={kit}&actor={actor}&handle=ada@nest.example")
        );
    }

    /// A minted kit's `Debug` must not print the recovery root, for
    /// [`FfiLandedSuccession`]'s reason.
    #[test]
    fn debug_never_prints_a_minted_kit() {
        let secret = "ef".repeat(32);
        let rendered = format!(
            "{:?}",
            FfiMintedKit {
                secret_hex: secret.clone(),
                escrow_stored: false,
                lands_at: None,
            }
        );
        assert!(!rendered.contains(&secret), "kit leaked: {rendered}");
        assert!(rendered.contains("<redacted>"));
    }

    /// A redacted `Debug` is the difference between a trace line and a leaked
    /// account: this record holds the successor seed at the one moment it exists
    /// nowhere else.
    #[test]
    fn debug_never_prints_the_successor_seed() {
        let secret = "cd".repeat(32);
        let landed = LandedSuccession::new(
            secret.clone(),
            fauna_core::identity::ActorId([2u8; 32]),
            SweepStatus::NoEngine,
            None,
        );
        let rendered = format!("{:?}", landed_view(&landed, true));
        assert!(!rendered.contains(&secret), "seed leaked: {rendered}");
        assert!(rendered.contains("<redacted>"));
    }

    /// An account store whose writes succeed by signature and vanish — the
    /// failure the read-back exists for (`SecretStore::set` is infallible, so
    /// `add_account` returning `Ok` proves nothing). `drop_writes: false` is an
    /// ordinary in-memory store.
    struct SeedStore {
        drop_writes: bool,
        map: std::sync::Mutex<std::collections::HashMap<String, String>>,
    }

    impl crate::accounts_registry::FfiSecretStore for SeedStore {
        fn get(&self, key: String) -> Option<String> {
            self.map.lock().unwrap().get(&key).cloned()
        }
        fn set(&self, key: String, value: String) {
            if !self.drop_writes {
                self.map.lock().unwrap().insert(key, value);
            }
        }
        fn delete(&self, key: String) {
            self.map.lock().unwrap().remove(&key);
        }
    }

    fn persist_over(drop_writes: bool) -> bool {
        let accounts = FfiAccountRegistry::new(Arc::new(SeedStore {
            drop_writes,
            map: Default::default(),
        }));
        let secret = "ab".repeat(32);
        let actor = ActorKeypair::from_secret_hex(&secret)
            .unwrap()
            .actor_id_hex();
        persist_successor_seed(accounts.registry(), &secret, &actor, "https://nest.example")
    }

    /// The succession ceremony's read-back verdict (`identity-succession.md`
    /// § Implementation status today): over a store whose writes never land,
    /// the seed must NOT be reported saved — an app believing it would tear
    /// the session down and take the only copy of the seed with it.
    #[test]
    fn a_successor_seed_that_does_not_read_back_is_not_reported_saved() {
        assert!(!persist_over(true));
    }

    /// The companion: over a working store the seed reads back and is
    /// reported saved, so a mutant that never believes the store reddens too.
    #[test]
    fn a_successor_seed_that_reads_back_is_reported_saved() {
        assert!(persist_over(false));
    }
}
